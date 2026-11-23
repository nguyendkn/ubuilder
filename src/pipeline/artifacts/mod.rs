//! Content-addressed pipeline artifact store with run/name metadata.

use crate::artifacts::{copy_tree, hash_path};
use crate::error::{ErrorCode, Result, UbuilderError};
use crate::pipeline::expression::pattern_matches;
use crate::storage::{Db, PipelineArtifactRecord};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use walkdir::WalkDir;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub name: String,
    pub sha256: String,
    pub size: u64,
    pub path: PathBuf,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ObjectManifest {
    kind: String,
    root_name: String,
    sha256: String,
    size: u64,
}

pub struct ArtifactStore {
    root: PathBuf,
    db: Arc<Db>,
}

impl ArtifactStore {
    pub fn open(root: &Path, db: Arc<Db>) -> Result<Self> {
        std::fs::create_dir_all(root.join("objects"))
            .map_err(|e| UbuilderError::io("cannot create artifact store", e))?;
        std::fs::create_dir_all(root.join("tmp"))
            .map_err(|e| UbuilderError::io("cannot create artifact temp dir", e))?;
        Ok(Self {
            root: root.to_path_buf(),
            db,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn upload(
        &self,
        run_id: &str,
        job_id: &str,
        name: &str,
        source: &Path,
    ) -> Result<ArtifactRef> {
        validate_name(name)?;
        if !source.exists() {
            return Err(UbuilderError::new(
                ErrorCode::ArtifactInvalid,
                format!("artifact path {} does not exist", source.display()),
            ));
        }
        let kind = if source.is_dir() { "directory" } else { "file" };
        let sha256 = hash_path(source)?;
        let object = self.root.join("objects").join(&sha256);
        let tree = object.join("tree");
        let manifest_path = object.join("manifest.json");
        if !manifest_path.is_file() {
            let staging = self
                .root
                .join("tmp")
                .join(format!("{}-{}", sha256, std::process::id()));
            let _ = std::fs::remove_dir_all(&staging);
            std::fs::create_dir_all(staging.join("tree"))
                .map_err(|e| UbuilderError::io("cannot create artifact staging", e))?;
            let root_name = source
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("artifact")
                .to_string();
            let payload = staging.join("tree").join(&root_name);
            if source.is_dir() {
                copy_tree(source, &payload)?;
            } else {
                std::fs::copy(source, &payload).map_err(|e| {
                    UbuilderError::io(format!("cannot copy {}", source.display()), e)
                })?;
            }
            let size = tree_size(&payload)?;
            let manifest = ObjectManifest {
                kind: kind.into(),
                root_name,
                sha256: sha256.clone(),
                size,
            };
            std::fs::write(
                staging.join("manifest.json"),
                serde_json::to_vec_pretty(&manifest)
                    .map_err(|e| UbuilderError::new(ErrorCode::Internal, e.to_string()))?,
            )
            .map_err(|e| UbuilderError::io("cannot write artifact manifest", e))?;
            let _ = std::fs::remove_dir_all(&object);
            std::fs::rename(&staging, &object)
                .map_err(|e| UbuilderError::io("cannot publish artifact object", e))?;
        }
        let manifest = self.manifest(&sha256)?;
        let record = PipelineArtifactRecord {
            id: None,
            run_id: run_id.into(),
            job_id: job_id.into(),
            name: name.into(),
            path: tree,
            sha256: sha256.clone(),
            size: manifest.size,
            created_at: crate::telemetry::now_ms(),
        };
        self.db.add_pipeline_artifact(&record)?;
        Ok(ArtifactRef {
            name: name.into(),
            sha256,
            size: manifest.size,
            path: record.path,
            kind: manifest.kind,
        })
    }

    pub fn list(&self, run_id: &str) -> Result<Vec<PipelineArtifactRecord>> {
        self.db.pipeline_artifacts(run_id)
    }

    pub fn download(
        &self,
        run_id: &str,
        name_pattern: &str,
        destination: &Path,
    ) -> Result<Vec<PathBuf>> {
        let artifacts = self.list(run_id)?;
        let matching: Vec<_> = artifacts
            .iter()
            .filter(|artifact| pattern_matches(name_pattern, &artifact.name))
            .collect();
        if matching.is_empty() {
            return Err(UbuilderError::new(
                ErrorCode::ArtifactInvalid,
                format!("no artifact matching '{name_pattern}' in run {run_id}"),
            )
            .remediation("Upload it in a job that runs before this job"));
        }
        std::fs::create_dir_all(destination)
            .map_err(|e| UbuilderError::io("cannot create download destination", e))?;
        let mut paths = Vec::new();
        for artifact in matching {
            let manifest = self.manifest(&artifact.sha256)?;
            let tree = self
                .root
                .join("objects")
                .join(&artifact.sha256)
                .join("tree");
            let source = tree.join(&manifest.root_name);
            let target_root = if name_pattern == artifact.name
                && matching_len(&self.list(run_id)?, name_pattern) == 1
            {
                destination.to_path_buf()
            } else {
                destination.join(safe_filename(&artifact.name))
            };
            if manifest.kind == "directory" {
                copy_tree(&source, &target_root)?;
                paths.push(target_root);
            } else {
                std::fs::create_dir_all(&target_root)
                    .map_err(|e| UbuilderError::io("cannot create download destination", e))?;
                let target = target_root.join(&manifest.root_name);
                std::fs::copy(&source, &target)
                    .map_err(|e| UbuilderError::io("cannot copy downloaded artifact", e))?;
                paths.push(target);
            }
        }
        Ok(paths)
    }

    fn manifest(&self, sha256: &str) -> Result<ObjectManifest> {
        let path = self.root.join("objects").join(sha256).join("manifest.json");
        let bytes = std::fs::read(&path).map_err(|e| {
            UbuilderError::new(
                ErrorCode::ArtifactInvalid,
                format!("artifact object {} is missing: {e}", path.display()),
            )
        })?;
        serde_json::from_slice(&bytes).map_err(|e| {
            UbuilderError::new(
                ErrorCode::ArtifactInvalid,
                format!("corrupt artifact manifest {}: {e}", path.display()),
            )
        })
    }
}

fn matching_len(artifacts: &[PipelineArtifactRecord], pattern: &str) -> usize {
    artifacts
        .iter()
        .filter(|artifact| pattern_matches(pattern, &artifact.name))
        .count()
}

pub fn validate_name(name: &str) -> Result<()> {
    if name.trim().is_empty() || name.len() > 128 {
        return Err(UbuilderError::new(
            ErrorCode::ConfigInvalid,
            "artifact name must be 1-128 characters",
        ));
    }
    if name.contains('/') || name.contains('\\') || name == "." || name == ".." {
        return Err(UbuilderError::new(
            ErrorCode::ConfigInvalid,
            format!("artifact name '{name}' must not contain path separators"),
        ));
    }
    Ok(())
}

pub fn safe_filename(name: &str) -> String {
    name.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

/// Reject absolute paths and `..` components; then verify parent canonical paths
/// remain under the workspace when the path exists.
pub fn safe_join(root: &Path, relative: &str) -> Result<PathBuf> {
    let candidate = Path::new(relative);
    if candidate.is_absolute() {
        return Err(UbuilderError::new(
            ErrorCode::ConfigInvalid,
            format!("path '{relative}' must be relative to the job workspace"),
        ));
    }
    for component in candidate.components() {
        if matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            return Err(UbuilderError::new(
                ErrorCode::ConfigInvalid,
                format!("path '{relative}' escapes the job workspace"),
            ));
        }
    }
    Ok(root.join(candidate))
}

fn tree_size(path: &Path) -> Result<u64> {
    if path.is_file() {
        return std::fs::metadata(path)
            .map(|m| m.len())
            .map_err(|e| UbuilderError::io("cannot stat artifact", e));
    }
    let mut total = 0u64;
    for entry in WalkDir::new(path).follow_links(false).into_iter().flatten() {
        if entry.file_type().is_file() {
            total += entry.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_cannot_escape() {
        assert!(validate_name("hello").is_ok());
        assert!(validate_name("../hello").is_err());
        assert!(validate_name("a/b").is_err());
    }

    #[test]
    fn joins_are_confined() {
        let root = Path::new("workspace");
        assert_eq!(safe_join(root, "dist/a").unwrap(), root.join("dist/a"));
        assert!(safe_join(root, "../elsewhere").is_err());
        assert!(safe_join(root, "C:\\temp").is_err());
    }
}
