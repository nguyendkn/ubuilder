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
}

impl PipelineCache {
    pub fn open(root: &Path, enabled: bool, host: &str, _ir: &WorkflowIr) -> Result<Self> {
        Ok(Self {
            cache: Cache::open(root, enabled)?,
            host: host.into(),
        })
    }

    pub fn inner(&self) -> &Cache {
        &self.cache
    }

    /// Namespaced, host-specific key. The key itself is hashed so arbitrary
    /// user text never becomes a path.
    pub fn key(&self, user_key: &str) -> String {
        // The user key is the step-level fingerprint. Workflow-wide fields are
        // deliberately excluded: editing an unrelated job must not evict this
        // step's cache. The key contract still requires callers to include
        // toolchain/version inputs that affect the restored directory.
        let prefix = format!("pipeline/{}/{}", CACHE_ACTION_VERSION, self.host);
        crate::artifacts::sha256_bytes(format!("{prefix}\n{user_key}").as_bytes())
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
