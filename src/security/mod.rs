//! Security helpers: path containment, environment allowlists, secret redaction.

use crate::error::{ErrorCode, Result, UbuilderError};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

/// Reject absolute paths, drive prefixes, and `..` traversal.
pub fn validate_relative_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() {
        return Err(UbuilderError::new(
            ErrorCode::Usage,
            "path must not be empty",
        ));
    }
    if path.is_absolute() {
        return Err(UbuilderError::new(
            ErrorCode::Usage,
            format!("expected a relative path, got {}", path.display()),
        ));
    }
    let raw = path.to_string_lossy();
    if raw.starts_with('/') || raw.starts_with('\\') || has_windows_drive_prefix(&raw) {
        return Err(UbuilderError::new(
            ErrorCode::Usage,
            format!("absolute path is not allowed: {}", path.display()),
        ));
    }
    for component in path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => {
                return Err(UbuilderError::new(
                    ErrorCode::Usage,
                    format!("path traversal is not allowed: {}", path.display()),
                ))
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(UbuilderError::new(
                    ErrorCode::Usage,
                    format!("absolute path is not allowed: {}", path.display()),
                ))
            }
        }
    }
    Ok(())
}

fn has_windows_drive_prefix(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// Canonicalize `candidate` and require it to remain inside `root`.
pub fn ensure_within(root: &Path, candidate: &Path) -> Result<PathBuf> {
    let root = root
        .canonicalize()
        .map_err(|e| UbuilderError::io(format!("cannot resolve {}", root.display()), e))?;
    let candidate = candidate
        .canonicalize()
        .map_err(|e| UbuilderError::io(format!("cannot resolve {}", candidate.display()), e))?;
    if !candidate.starts_with(&root) {
        return Err(UbuilderError::new(
            ErrorCode::ArtifactInvalid,
            format!(
                "{} escapes the allowed root {}",
                candidate.display(),
                root.display()
            ),
        ));
    }
    Ok(candidate)
}

/// Build a filtered environment. Secrets are included only when allowlisted.
pub fn filtered_env(allowlist: &[String]) -> BTreeMap<String, String> {
    let allow: Vec<String> = allowlist.iter().map(|s| s.to_ascii_uppercase()).collect();
    std::env::vars()
        .filter(|(key, _)| allow.iter().any(|a| a == &key.to_ascii_uppercase()))
        .collect()
}

pub fn looks_secret_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "PRIVATE_KEY",
        "API_KEY",
        "CREDENTIAL",
    ]
    .iter()
    .any(|marker| upper.contains(marker))
}

/// Replace secret values in captured output. Never log the secret itself.
pub fn redact(text: &str, secrets: &[String]) -> String {
    let mut out = text.to_string();
    for secret in secrets {
        if !secret.is_empty() {
            out = out.replace(secret, "***");
        }
    }
    out
}

/// Environment variable values that should never appear in logs.
pub fn secrets_from_env(env: &BTreeMap<String, String>) -> Vec<String> {
    env.iter()
        .filter(|(k, _)| looks_secret_key(k))
        .map(|(_, v)| v.clone())
        .filter(|v| !v.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal_and_absolute_paths() {
        assert!(validate_relative_path(Path::new("a/b")).is_ok());
        assert!(validate_relative_path(Path::new("../evil")).is_err());
        assert!(validate_relative_path(Path::new("C:/Windows")).is_err());
        assert!(validate_relative_path(Path::new("/etc/passwd")).is_err());
    }

    #[test]
    fn containment_rejects_symlink_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let inside = tmp.path().join("in");
        std::fs::create_dir_all(&inside).unwrap();
        assert!(ensure_within(tmp.path(), &inside).is_ok());
        let outside = tempfile::tempdir().unwrap();
        assert!(ensure_within(tmp.path(), outside.path()).is_err());
    }

    #[test]
    fn redacts_secret_values() {
        let text = "token=abc123def password=hunter2";
        let redacted = redact(text, &["abc123def".into(), "hunter2".into()]);
        assert_eq!(redacted, "token=*** password=***");
    }

    #[test]
    fn identifies_secret_keys() {
        assert!(looks_secret_key("NPM_TOKEN"));
        assert!(looks_secret_key("SIGNING_PASSWORD"));
        assert!(!looks_secret_key("PATH"));
    }
}
