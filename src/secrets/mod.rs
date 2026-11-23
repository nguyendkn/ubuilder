//! Local-first secret resolution.
//!
//! Secrets are resolved lazily at execution time. Values never enter the IR,
//! plans, SQLite rows, JSON reports or cache keys; callers receive values only
//! for injection into a child environment and for redaction.
//!
//! Providers are intentionally local: environment variables and a protected
//! user data file. A file provider is not an HSM; on Unix the file is mode
//! 0600 under a 0700 directory. Windows inherits the user's profile ACL.

use crate::error::{ErrorCode, Result, UbuilderError};
use crate::security;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

pub const SECRET_FILE: &str = "secrets.json";
pub const ENV_PREFIX: &str = "UBUILDER_SECRET_";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretReference {
    pub name: String,
}

impl SecretReference {
    pub fn new(name: impl Into<String>) -> Result<Self> {
        let name = name.into();
        validate_name(&name)?;
        Ok(Self { name })
    }
}

/// A resolved value. `Debug` never prints the value.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretValue(String);

impl SecretValue {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretValue(***)")
    }
}

pub trait SecretProvider: Send + Sync {
    fn id(&self) -> &'static str;
    fn resolve(&self, reference: &SecretReference) -> Result<Option<SecretValue>>;
    fn list(&self) -> Result<Vec<String>>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct EnvironmentSecretProvider;

impl EnvironmentSecretProvider {
    fn env_key(name: &str) -> String {
        format!("{ENV_PREFIX}{name}")
    }
}

impl SecretProvider for EnvironmentSecretProvider {
    fn id(&self) -> &'static str {
        "environment"
    }

    fn resolve(&self, reference: &SecretReference) -> Result<Option<SecretValue>> {
        // A prefixed variable is preferred so operators can keep workflow
        // secrets separate from unrelated process variables. A direct match is
        // accepted for CI systems that already export the canonical name.
        for key in [Self::env_key(&reference.name), reference.name.clone()] {
            if let Ok(value) = std::env::var(&key) {
                if !value.is_empty() {
                    return Ok(Some(SecretValue::new(value)));
                }
            }
        }
        Ok(None)
    }

    fn list(&self) -> Result<Vec<String>> {
        let mut names: Vec<String> = std::env::vars_os()
            .filter_map(|(key, _)| key.into_string().ok())
            .filter_map(|key| key.strip_prefix(ENV_PREFIX).map(str::to_string))
            .filter(|name| validate_name(name).is_ok())
            .collect();
        names.sort();
        names.dedup();
        Ok(names)
    }
}

#[derive(Debug, Clone)]
pub struct FileSecretProvider {
    path: PathBuf,
}

impl FileSecretProvider {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read(&self) -> Result<BTreeMap<String, String>> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(BTreeMap::new())
            }
            Err(error) => {
                return Err(UbuilderError::io(
                    format!("cannot read secret file {}", self.path.display()),
                    error,
                ))
            }
        };
        serde_json::from_slice(&bytes).map_err(|error| {
            UbuilderError::new(
                ErrorCode::ConfigInvalid,
                format!("secret file {} is not valid JSON", self.path.display()),
            )
            .remediation("Restore the file or recreate secrets with `ubuilder secret set`")
            .details(serde_json::json!({ "error": error.to_string() }))
        })
    }

    fn write(&self, entries: &BTreeMap<String, String>) -> Result<()> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| UbuilderError::new(ErrorCode::Internal, "secret file has no parent"))?;
        std::fs::create_dir_all(parent).map_err(|error| {
            UbuilderError::io(
                format!("cannot create secret directory {}", parent.display()),
                error,
            )
        })?;
        restrict_dir(parent)?;
        let lock_path = self.path.with_extension("json.lock");
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|error| {
                UbuilderError::io(
                    format!("cannot open secret lock {}", lock_path.display()),
                    error,
                )
            })?;
        lock.lock_exclusive()
            .map_err(|error| UbuilderError::io("cannot lock secret file", error))?;
        let result = self.write_locked(entries);
        let _ = fs2::FileExt::unlock(&lock);
        result
    }

    fn write_locked(&self, entries: &BTreeMap<String, String>) -> Result<()> {
        let temp = self
            .path
            .with_extension(format!("json.tmp-{}", std::process::id()));
        let bytes = serde_json::to_vec_pretty(entries)
            .map_err(|error| UbuilderError::new(ErrorCode::Internal, error.to_string()))?;
        {
            use std::io::Write;
            let mut file = std::fs::File::create(&temp).map_err(|error| {
                UbuilderError::io(format!("cannot write {}", temp.display()), error)
            })?;
            file.write_all(&bytes)
                .map_err(|error| UbuilderError::io("cannot write secret file", error))?;
            file.sync_all()
                .map_err(|error| UbuilderError::io("cannot sync secret file", error))?;
            restrict_file(&temp)?;
        }
        std::fs::rename(&temp, &self.path).map_err(|error| {
            let _ = std::fs::remove_file(&temp);
            UbuilderError::io(
                format!("cannot publish secret file {}", self.path.display()),
                error,
            )
        })?;
        Ok(())
    }
}

impl SecretProvider for FileSecretProvider {
    fn id(&self) -> &'static str {
        "file"
    }

    fn resolve(&self, reference: &SecretReference) -> Result<Option<SecretValue>> {
        Ok(self
            .read()?
            .get(&reference.name)
            .filter(|value| !value.is_empty())
            .cloned()
            .map(SecretValue::new))
    }

    fn list(&self) -> Result<Vec<String>> {
        let mut names: Vec<String> = self.read()?.into_keys().collect();
        names.sort();
        Ok(names)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretInfo {
    pub name: String,
    pub provider: String,
    /// Character count only; never the value.
    pub length: usize,
    pub writable: bool,
}

/// Ordered provider chain. Earlier providers win.
pub struct SecretManager {
    providers: Vec<Box<dyn SecretProvider>>,
    file: FileSecretProvider,
}

impl SecretManager {
    pub fn open(data_dir: &Path) -> Result<Self> {
        let file = FileSecretProvider::new(data_dir.join(SECRET_FILE));
        Ok(Self {
            providers: vec![Box::new(EnvironmentSecretProvider), Box::new(file.clone())],
            file,
        })
    }

    pub fn file_path(&self) -> &Path {
        self.file.path()
    }

    pub fn resolve(&self, name: &str) -> Result<Option<(SecretValue, &'static str)>> {
        let reference = SecretReference::new(name)?;
        for provider in &self.providers {
            if let Some(value) = provider.resolve(&reference)? {
                return Ok(Some((value, provider.id())));
            }
        }
        Ok(None)
    }

    pub fn resolve_required(&self, name: &str) -> Result<SecretValue> {
        self.resolve(name)?.map(|(value, _)| value).ok_or_else(|| {
            UbuilderError::new(
                ErrorCode::ConfigInvalid,
                format!("secret '{name}' is not available"),
            )
            .remediation(format!(
                "Set it with `ubuilder secret set {name}` or {ENV_PREFIX}{name}"
            ))
        })
    }

    /// Resolve only names that appear as `secrets.<name>` in an IR or workflow
    /// representation. Missing names are errors so a typo cannot silently
    /// inject an empty value.
    pub fn resolve_referenced(
        &self,
        representation: &str,
    ) -> Result<BTreeMap<String, SecretValue>> {
        let mut values = BTreeMap::new();
        for name in referenced_secret_names(representation) {
            values.insert(name.clone(), self.resolve_required(&name)?);
        }
        Ok(values)
    }

    pub fn list(&self) -> Result<Vec<SecretInfo>> {
        let mut merged: BTreeMap<String, SecretInfo> = BTreeMap::new();
        for provider in &self.providers {
            for name in provider.list()? {
                let reference = match SecretReference::new(&name) {
                    Ok(reference) => reference,
                    Err(_) => continue,
                };
                let Some(value) = provider.resolve(&reference)? else {
                    continue;
                };
                merged.entry(name.clone()).or_insert(SecretInfo {
                    name,
                    provider: provider.id().into(),
                    length: value.expose().chars().count(),
                    writable: provider.id() == "file",
                });
            }
        }
        Ok(merged.into_values().collect())
    }

    pub fn inspect(&self, name: &str) -> Result<Option<SecretInfo>> {
        let reference = SecretReference::new(name)?;
        for provider in &self.providers {
            if let Some(value) = provider.resolve(&reference)? {
                return Ok(Some(SecretInfo {
                    name: name.into(),
                    provider: provider.id().into(),
                    length: value.expose().chars().count(),
                    writable: provider.id() == "file",
                }));
            }
        }
        Ok(None)
    }

    pub fn set_file(&self, name: &str, value: &str) -> Result<()> {
        let reference = SecretReference::new(name)?;
        if value.is_empty() {
            return Err(UbuilderError::new(
                ErrorCode::Usage,
                format!("secret '{}' must not be empty", reference.name),
            ));
        }
        let mut entries = self.file.read()?;
        entries.insert(reference.name.clone(), value.to_string());
        self.file.write(&entries)
    }

    pub fn delete_file(&self, name: &str) -> Result<bool> {
        let reference = SecretReference::new(name)?;
        let mut entries = self.file.read()?;
        let removed = entries.remove(&reference.name).is_some();
        if removed {
            self.file.write(&entries)?;
        }
        Ok(removed)
    }
}

pub fn validate_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'));
    if valid {
        Ok(())
    } else {
        Err(
            UbuilderError::new(ErrorCode::Usage, format!("invalid secret name '{name}'"))
                .remediation(
                    "Use 1-128 characters starting with a letter or _, then [A-Za-z0-9_-]",
                ),
        )
    }
}

/// Extract identifiers from `secrets.<identifier>` occurrences. This is a
/// lexical scan over an already-compiled representation, not a second parser.
pub fn referenced_secret_names(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = text;
    while let Some(index) = rest.find("secrets.") {
        let after = &rest[index + "secrets.".len()..];
        let end = after
            .char_indices()
            .take_while(|(_, ch)| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
            .map(|(offset, ch)| offset + ch.len_utf8())
            .last()
            .unwrap_or(0);
        if end > 0 {
            let candidate = &after[..end];
            if validate_name(candidate).is_ok() && !names.iter().any(|name| name == candidate) {
                names.push(candidate.to_string());
            }
        }
        rest = &after[end..];
    }
    names.sort();
    names
}

/// Values to feed the executor's redactor.
pub fn values(map: &BTreeMap<String, SecretValue>) -> Vec<String> {
    map.values()
        .map(|value| value.expose().to_string())
        .collect()
}

pub fn redact(text: &str, map: &BTreeMap<String, SecretValue>) -> String {
    security::redact(text, &values(map))
}

pub struct SecretSet {
    values: BTreeMap<String, SecretValue>,
}

impl SecretSet {
    pub fn new(values: BTreeMap<String, SecretValue>) -> Self {
        Self { values }
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn map(&self) -> BTreeMap<String, String> {
        self.values
            .iter()
            .map(|(name, value)| (name.clone(), value.expose().to_string()))
            .collect()
    }

    pub fn values(&self) -> Vec<String> {
        values(&self.values)
    }

    pub fn redact(&self, text: &str) -> String {
        redact(text, &self.values)
    }
}

#[cfg(unix)]
fn restrict_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| UbuilderError::io("cannot secure secret file permissions", error))
}

#[cfg(not(unix))]
fn restrict_file(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn restrict_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| UbuilderError::io("cannot secure secret directory permissions", error))
}

#[cfg(not(unix))]
fn restrict_dir(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_prints_value() {
        let value = SecretValue::new("super-secret");
        assert_eq!(format!("{value:?}"), "SecretValue(***)");
    }

    #[test]
    fn extracts_references_without_values() {
        let names = referenced_secret_names(
            "env: TOKEN=${{ secrets.SIGNING_TOKEN }}\nother=${{secrets.API_KEY}}",
        );
        assert_eq!(names, vec!["API_KEY", "SIGNING_TOKEN"]);
    }

    #[test]
    fn file_provider_roundtrips() {
        let temp = tempfile::tempdir().unwrap();
        let provider = FileSecretProvider::new(temp.path().join(SECRET_FILE));
        let reference = SecretReference::new("TOKEN").unwrap();
        assert!(provider.resolve(&reference).unwrap().is_none());
        let mut entries = BTreeMap::new();
        entries.insert("TOKEN".into(), "abc123".into());
        provider.write(&entries).unwrap();
        assert_eq!(
            provider.resolve(&reference).unwrap().unwrap().expose(),
            "abc123"
        );
        assert_eq!(provider.list().unwrap(), vec!["TOKEN"]);
    }

    #[test]
    fn rejects_bad_names() {
        assert!(validate_name("TOKEN").is_ok());
        assert!(validate_name("_TOKEN-1").is_ok());
        assert!(validate_name("1TOKEN").is_err());
        assert!(validate_name("../TOKEN").is_err());
        assert!(validate_name("").is_err());
    }
}
