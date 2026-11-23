//! Content hashing, directory copying, deterministic ZIP creation and artifact
//! metadata recording.

use crate::error::{ErrorCode, Result, UbuilderError};
use crate::storage::{ArtifactRecord, Db};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipWriter};

pub const RECIPE_VERSION: u32 = 1;

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)
        .map_err(|e| UbuilderError::io(format!("cannot open {}", path.display()), e))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|e| UbuilderError::io(format!("cannot read {}", path.display()), e))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Deterministic hash of a file or directory tree (relative paths + contents).
pub fn hash_path(path: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    if path.is_file() {
        hasher.update(b"file\0");
        hasher.update(sha256_file(path)?.as_bytes());
        return Ok(hex::encode(hasher.finalize()));
    }
    if !path.is_dir() {
        return Err(UbuilderError::new(
            ErrorCode::ArtifactInvalid,
            format!("cannot hash missing path {}", path.display()),
        ));
    }
    for entry in WalkDir::new(path)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .flatten()
    {
        let rel = entry
            .path()
            .strip_prefix(path)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .replace('\\', "/");
        if entry.file_type().is_dir() {
            hasher.update(format!("dir\0{rel}\0").as_bytes());
        } else if entry.file_type().is_file() {
            hasher.update(format!("file\0{rel}\0").as_bytes());
            let content_hash = sha256_file(entry.path())?;
            hasher.update(content_hash.as_bytes());
        }
    }
    Ok(hex::encode(hasher.finalize()))
}

pub fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    if !from.exists() {
        return Err(UbuilderError::new(
            ErrorCode::ArtifactInvalid,
            format!("source does not exist: {}", from.display()),
        ));
    }
    std::fs::create_dir_all(to)
        .map_err(|e| UbuilderError::io(format!("cannot create {}", to.display()), e))?;
    let canonical_from = from
        .canonicalize()
        .map_err(|e| UbuilderError::io("cannot resolve source root", e))?;
    for entry in WalkDir::new(from)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .flatten()
    {
        let rel = entry.path().strip_prefix(from).unwrap_or(entry.path());
        if rel.as_os_str().is_empty() {
            continue;
        }
        let target = to.join(rel);
        let file_type = entry.file_type();
        if file_type.is_dir() {
            std::fs::create_dir_all(&target)
                .map_err(|e| UbuilderError::io(format!("cannot create {}", target.display()), e))?;
        } else if file_type.is_file() {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    UbuilderError::io(format!("cannot create {}", parent.display()), e)
                })?;
            }
            std::fs::copy(entry.path(), &target).map_err(|e| {
                UbuilderError::io(
                    format!(
                        "cannot copy {} -> {}",
                        entry.path().display(),
                        target.display()
                    ),
                    e,
                )
            })?;
        } else if file_type.is_symlink() {
            // Copy the resolved file content only if it stays reachable; a symlink
            // escaping the project is a security concern during packaging.
            let resolved = entry
                .path()
                .canonicalize()
                .map_err(|e| UbuilderError::io("cannot resolve symlink", e))?;
            if !resolved.starts_with(&canonical_from) {
                return Err(UbuilderError::new(
                    ErrorCode::ArtifactInvalid,
                    format!(
                        "symlink {} escapes the packaged root {}",
                        entry.path().display(),
                        from.display()
                    ),
                )
                .remediation("Copy the target inside the project or remove the symlink"));
            }
            if resolved.is_file() {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        UbuilderError::io(format!("cannot create {}", parent.display()), e)
                    })?;
                }
                std::fs::copy(&resolved, &target).map_err(|e| {
                    UbuilderError::io(
                        format!("cannot copy symlink target {}", resolved.display()),
                        e,
                    )
                })?;
            }
        }
    }
    Ok(())
}

pub fn copy_any(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| UbuilderError::io(format!("cannot create {}", parent.display()), e))?;
    }
    if from.is_dir() {
        copy_tree(from, to)
    } else {
        std::fs::copy(from, to)
            .map(|_| ())
            .map_err(|e| UbuilderError::io(format!("cannot copy {}", from.display()), e))
    }
}

pub fn atomic_replace(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| UbuilderError::io(format!("cannot create {}", parent.display()), e))?;
    }
    #[cfg(windows)]
    {
        if to.exists() {
            std::fs::remove_file(to)
                .map_err(|e| UbuilderError::io(format!("cannot replace {}", to.display()), e))?;
        }
    }
    std::fs::rename(from, to)
        .map_err(|e| UbuilderError::io(format!("cannot rename into {}", to.display()), e))
}

/// Create a deterministic ZIP (sorted entries, fixed timestamps).
pub fn zip_directory(src: &Path, dest: &Path) -> Result<u64> {
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
        let name = rel.to_string_lossy().replace('\\', "/");
        if entry.file_type().is_dir() {
            zip.add_directory(format!("{name}/"), options)
                .map_err(|e| UbuilderError::new(ErrorCode::Io, format!("zip dir failed: {e}")))?;
        } else if entry.file_type().is_file() {
            zip.start_file(&name, options)
                .map_err(|e| UbuilderError::new(ErrorCode::Io, format!("zip entry failed: {e}")))?;
            let mut input = File::open(entry.path()).map_err(|e| {
                UbuilderError::io(format!("cannot read {}", entry.path().display()), e)
            })?;
            std::io::copy(&mut input, &mut zip)
                .map_err(|e| UbuilderError::new(ErrorCode::Io, format!("zip write failed: {e}")))?;
        }
    }
    let mut file = zip
        .finish()
        .map_err(|e| UbuilderError::new(ErrorCode::Io, format!("zip finalize failed: {e}")))?;
    file.flush()
        .map_err(|e| UbuilderError::io(format!("cannot flush {}", tmp.display()), e))?;
    drop(file);
    atomic_replace(&tmp, dest)?;
    Ok(std::fs::metadata(dest)
        .map_err(|e| UbuilderError::io(format!("cannot stat {}", dest.display()), e))?
        .len())
}

pub fn list_zip(dest: &Path) -> Result<Vec<String>> {
    let file = File::open(dest)
        .map_err(|e| UbuilderError::io(format!("cannot open {}", dest.display()), e))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| UbuilderError::new(ErrorCode::ArtifactInvalid, format!("invalid zip: {e}")))?;
    let mut names = Vec::new();
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|e| {
            UbuilderError::new(ErrorCode::ArtifactInvalid, format!("bad zip entry: {e}"))
        })?;
        names.push(entry.name().to_string());
    }
    Ok(names)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactSummary {
    pub path: PathBuf,
    pub sha256: String,
    pub size: u64,
    pub kind: String,
    pub format: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildManifest {
    pub schema_version: u32,
    pub build_id: String,
    pub project: String,
    pub framework: String,
    pub host: String,
    pub target: String,
    pub profile: String,
    pub started_at: u128,
    pub finished_at: u128,
    pub status: String,
    pub steps: Vec<ManifestStep>,
    pub artifacts: Vec<ArtifactSummary>,
    pub validations: Vec<serde_json::Value>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestStep {
    pub id: String,
    pub name: String,
    pub status: String,
    pub exit_code: Option<i32>,
    pub duration_ms: u128,
    pub log_path: Option<PathBuf>,
    pub error: Option<String>,
}

impl BuildManifest {
    pub fn write(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| UbuilderError::io(format!("cannot create {}", parent.display()), e))?;
        }
        let json = serde_json::to_vec_pretty(self)
            .map_err(|e| UbuilderError::new(ErrorCode::Json, format!("manifest: {e}")))?;
        crate::artifacts::atomic_write(path, &json)
    }
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| UbuilderError::io(format!("cannot create {}", parent.display()), e))?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut file = File::create(&tmp)
            .map_err(|e| UbuilderError::io(format!("cannot create {}", tmp.display()), e))?;
        file.write_all(bytes)
            .map_err(|e| UbuilderError::io(format!("cannot write {}", tmp.display()), e))?;
        file.sync_all()
            .map_err(|e| UbuilderError::io(format!("cannot sync {}", tmp.display()), e))?;
    }
    atomic_replace(&tmp, path)
}

pub struct ArtifactStore<'a> {
    db: &'a Db,
}

impl<'a> ArtifactStore<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &self,
        job_id: &str,
        target: &str,
        framework: &str,
        kind: &str,
        format: &str,
        path: &Path,
        validation: Option<&str>,
    ) -> Result<ArtifactRecord> {
        let sha256 = if path.is_dir() {
            hash_path(path)?
        } else {
            sha256_file(path)?
        };
        let size = if path.is_dir() {
            WalkDir::new(path)
                .into_iter()
                .flatten()
                .filter(|e| e.file_type().is_file())
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum()
        } else {
            std::fs::metadata(path)
                .map_err(|e| UbuilderError::io(format!("cannot stat {}", path.display()), e))?
                .len()
        };
        let record = ArtifactRecord {
            id: None,
            job_id: job_id.to_string(),
            target: target.to_string(),
            framework: framework.to_string(),
            kind: kind.to_string(),
            format: format.to_string(),
            path: path.to_path_buf(),
            sha256: sha256.clone(),
            size,
            validation: validation.map(str::to_string),
            created_at: crate::telemetry::now_ms(),
        };
        let id = self.db.add_artifact(&record)?;
        Ok(ArtifactRecord {
            id: Some(id),
            ..record
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_stable_and_content_sensitive() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "hello").unwrap();
        let h1 = hash_path(tmp.path()).unwrap();
        let h2 = hash_path(tmp.path()).unwrap();
        assert_eq!(h1, h2);
        std::fs::write(tmp.path().join("a.txt"), "hello2").unwrap();
        assert_ne!(h1, hash_path(tmp.path()).unwrap());
    }

    #[test]
    fn zip_is_deterministic() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("a.bin"), b"abc").unwrap();
        std::fs::write(src.join("sub/b.txt"), b"def").unwrap();
        let z1 = tmp.path().join("one.zip");
        let z2 = tmp.path().join("two.zip");
        zip_directory(&src, &z1).unwrap();
        zip_directory(&src, &z2).unwrap();
        assert_eq!(sha256_file(&z1).unwrap(), sha256_file(&z2).unwrap());
        assert_eq!(list_zip(&z1).unwrap().len(), 3);
    }

    #[test]
    fn copy_tree_preserves_nested_files() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(src.join("nested")).unwrap();
        std::fs::write(src.join("nested/file.txt"), "x").unwrap();
        let dst = tmp.path().join("dst");
        copy_tree(&src, &dst).unwrap();
        assert_eq!(
            std::fs::read_to_string(dst.join("nested/file.txt")).unwrap(),
            "x"
        );
    }

    #[cfg(unix)]
    #[test]
    fn copy_tree_rejects_symlinks_escaping_the_root() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(tmp.path().join("outside.txt"), "secret").unwrap();
        std::os::unix::fs::symlink(tmp.path().join("outside.txt"), src.join("link")).unwrap();
        let error = copy_tree(&src, &tmp.path().join("dst")).unwrap_err();
        assert_eq!(error.code, ErrorCode::ArtifactInvalid);
    }
}
