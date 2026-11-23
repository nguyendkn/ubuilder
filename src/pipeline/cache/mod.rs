//! Pipeline cache facade over the existing action cache.

use crate::cache::Cache;
use crate::error::{ErrorCode, Result, UbuilderError};
use crate::pipeline::artifacts::safe_join;
use crate::pipeline::ir::WorkflowIr;
use std::path::{Path, PathBuf};

pub const CACHE_ACTION_VERSION: &str = "v1";

pub struct PipelineCache {
    cache: Cache,
    host: String,
    namespace: String,
    toolchain: String,
}

impl PipelineCache {
    pub fn open(
        root: &Path,
        enabled: bool,
        host: &str,
        ir: &WorkflowIr,
        toolchain_fingerprint: &str,
    ) -> Result<Self> {
        let source_name = ir
            .source_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "workflow.yml".into());
        // Namespace the cache by workflow identity, not by checkout path, so
        // the same workflow shares entries across directories.
        let namespace =
            crate::artifacts::sha256_bytes(format!("{}\0{source_name}", ir.name).as_bytes());
        Ok(Self {
            cache: Cache::open(root, enabled)?,
            host: host.into(),
            namespace,
            toolchain: toolchain_fingerprint.into(),
        })
    }

    pub fn inner(&self) -> &Cache {
        &self.cache
    }

    /// Namespaced, host-specific key. The key itself is hashed so arbitrary
    /// user text never becomes a path.
    pub fn key(&self, user_key: &str) -> String {
        // Workflow-wide fields are deliberately excluded: editing an unrelated
        // job must not evict this step's cache. Everything that changes the
        // restored bytes is part of the identity: workflow namespace, action
        // format version, host, toolchain, and the action's own input digest.
        let prefix = format!(
            "pipeline/{}/{CACHE_ACTION_VERSION}/{}/{}\n",
            self.namespace, self.host, self.toolchain
        );
        crate::artifacts::sha256_bytes(format!("{prefix}{user_key}").as_bytes())
    }

    pub fn restore(&self, user_key: &str, workspace: &Path, relative: &str) -> Result<bool> {
        let key = self.key(user_key);
        let destination = safe_join(workspace, relative)?;
        self.cache.restore_tree(&key, &destination)
    }

    pub fn save(&self, user_key: &str, workspace: &Path, relative: &str) -> Result<()> {
        let key = self.key(user_key);
        let source = safe_join(workspace, relative)?;
        if !source.is_dir() {
            return Err(UbuilderError::new(
                ErrorCode::ConfigInvalid,
                format!("cache path '{}' must be a directory", source.display()),
            ));
        }
        self.cache.store_tree(&key, &source)
    }

    pub fn lookup(&self, user_key: &str) -> Result<bool> {
        Ok(self.cache.lookup(&self.key(user_key))?.is_some())
    }

    pub fn root(&self) -> PathBuf {
        self.cache.root().to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;

    fn ir(name: &str, source: &str) -> WorkflowIr {
        WorkflowIr {
            version: 1,
            name: name.into(),
            source_path: PathBuf::from(source),
            source_digest: "digest".into(),
            env: IndexMap::new(),
            security: Default::default(),
            jobs: Vec::new(),
            order: Vec::new(),
            waves: Vec::new(),
            max_matrix_jobs: 1,
        }
    }

    fn cache(root: &Path, ir: &WorkflowIr, toolchain: &str) -> PipelineCache {
        PipelineCache::open(root, true, "windows-x64", ir, toolchain).unwrap()
    }

    #[test]
    fn identity_layers_change_the_key() {
        let tmp = tempfile::tempdir().unwrap();
        let workflow = ir("CI", "a/ci.yml");
        let base = cache(tmp.path(), &workflow, "tc-1");
        let key = base.key("deps");

        assert_eq!(key, base.key("deps"));
        assert_ne!(key, base.key("deps-v2"));
        assert_ne!(key, cache(tmp.path(), &workflow, "tc-2").key("deps"));
        assert_ne!(
            key,
            cache(tmp.path(), &ir("Other", "a/ci.yml"), "tc-1").key("deps")
        );
        assert_ne!(
            key,
            cache(tmp.path(), &ir("CI", "a/other.yml"), "tc-1").key("deps")
        );

        // The checkout directory is not part of the identity.
        assert_eq!(
            key,
            cache(tmp.path(), &ir("CI", "elsewhere/ci.yml"), "tc-1").key("deps")
        );
    }

    #[test]
    fn corrupt_entry_is_not_restored() {
        let tmp = tempfile::tempdir().unwrap();
        let workflow = ir("CI", "ci.yml");
        let cache = cache(tmp.path(), &workflow, "tc-1");
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(workspace.join("deps")).unwrap();
        std::fs::write(workspace.join("deps/a"), "payload").unwrap();
        cache.save("deps", &workspace, "deps").unwrap();
        let stored = cache.inner().lookup(&cache.key("deps")).unwrap().unwrap();
        std::fs::write(stored.path, b"broken").unwrap();
        let destination = workspace.join("restored");
        let error = cache.restore("deps", &workspace, "restored").unwrap_err();
        assert_eq!(error.code, ErrorCode::CacheCorrupt);
        assert!(!destination.exists());
    }
}
