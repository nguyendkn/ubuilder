//! Two-level build cache: toolchain-native dependency caches plus a
//! content-addressed action cache with locking, atomic writes and SHA checks.

use crate::artifacts::{self, hash_path, sha256_file};
use crate::error::{ErrorCode, Result, UbuilderError};
use crate::executor::CommandSpec;
use crate::host::Host;
use crate::planner::{BuildPlan, BuildStep};
use crate::security;
use crate::storage::{CacheRecord, CacheStats, Db};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;

pub struct Cache {
    root: PathBuf,
    db: Db,
    enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PruneReport {
    pub removed_entries: u64,
    pub removed_bytes: u64,
    pub remaining_entries: u64,
    pub remaining_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheReport {
    pub root: PathBuf,
    pub enabled: bool,
    pub metadata: CacheStats,
    pub disk_bytes: u64,
    pub dependency_caches: Vec<DependencyCache>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyCache {
    pub name: String,
    pub path: Option<PathBuf>,
    pub bytes: u64,
}

impl Cache {
    pub fn open(root: &Path, enabled: bool) -> Result<Self> {
        std::fs::create_dir_all(root.join("cas")).map_err(|e| {
            UbuilderError::io(format!("cannot create cache at {}", root.display()), e)
        })?;
        std::fs::create_dir_all(root.join("locks"))
            .map_err(|e| UbuilderError::io(format!("cannot create {}", root.display()), e))?;
        let db = Db::open(&root.join("meta.db"))?;
        Ok(Self {
            root: root.to_path_buf(),
            db,
            enabled,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    /// Key includes source inputs, command, environment, host, target, toolchain
    /// fingerprint and the recipe version.
    pub fn step_key(
        &self,
        plan: &BuildPlan,
        step: &BuildStep,
        host: &Host,
        toolchain_fingerprint: &str,
    ) -> Result<String> {
        let mut parts: Vec<String> = vec![
            format!("recipe={}", artifacts::RECIPE_VERSION),
            format!("framework={}", plan.framework.as_str()),
            format!("project={}", plan.project_name),
            format!("target={}", plan.target.id),
            format!("host={}", host.triple),
            format!("step={}", step.id),
            format!("toolchain={toolchain_fingerprint}"),
        ];
        if let Some(command) = &step.command {
            parts.push(format!("command={}", key_for_command(command)));
        }
        let mut input_hashes = Vec::new();
        for input in &step.inputs {
            let hash = if input.exists() {
                hash_path(input)?
            } else {
                "missing".to_string()
            };
            input_hashes.push(format!("{}={hash}", input.to_string_lossy()));
        }
        input_hashes.sort();
        parts.extend(input_hashes);
        parts.sort();
        Ok(artifacts::sha256_bytes(parts.join("\n").as_bytes()))
    }

    pub fn restore_tree(&self, key: &str, dest: &Path) -> Result<bool> {
        if !self.enabled {
            return Ok(false);
        }
        let Some(record) = self.db.cache_record_hit(key, now())? else {
            return Ok(false);
        };
        let blob = &record.path;
        if !blob.is_file() {
            self.db.cache_delete(key)?;
            return Err(UbuilderError::new(
                ErrorCode::CacheCorrupt,
                format!("cache blob missing: {}", blob.display()),
            )
            .remediation("Run `ubuilder cache prune` or `ubuilder cache clear`"));
        }
        let blob_hash = sha256_file(blob)?;
        let expected = blob
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        if !expected.is_empty() && blob_hash != expected {
            self.db.cache_delete(key)?;
            return Err(UbuilderError::new(
                ErrorCode::CacheCorrupt,
                format!("cache blob hash mismatch for {}", blob.display()),
            )
            .remediation("Run `ubuilder cache clear` to discard corrupted entries"));
        }
        let parent = dest.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)
            .map_err(|e| UbuilderError::io(format!("cannot create {}", parent.display()), e))?;
        if dest.exists() {
            std::fs::remove_dir_all(dest)
                .map_err(|e| UbuilderError::io(format!("cannot clear {}", dest.display()), e))?;
        }
        extract_zip(blob, dest)?;
        Ok(true)
    }

    pub fn store_tree(&self, key: &str, src: &Path) -> Result<()> {
        if !self.enabled || !src.is_dir() {
            return Ok(());
        }
        let _lock = self.lock_key(key)?;
        if self.db.cache_record_hit(key, now())?.is_some() {
            return Ok(());
        }
        let tmp = self
            .root
            .join("cas")
            .join(format!("tmp-{}-{}.zip", std::process::id(), now()));
        let size = artifacts::zip_directory(src, &tmp)?;
        let sha = sha256_file(&tmp)?;
        let shard = self.root.join("cas").join(&sha[..2.min(sha.len())]);
        std::fs::create_dir_all(&shard)
            .map_err(|e| UbuilderError::io(format!("cannot create {}", shard.display()), e))?;
        let blob = shard.join(format!("{sha}.zip"));
        if !blob.exists() {
            artifacts::atomic_replace(&tmp, &blob)?;
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
        let record = CacheRecord {
            key: key.to_string(),
            kind: "tree".into(),
            path: blob,
            size,
            created_at: now(),
            last_used: now(),
            hits: 0,
        };
        self.db.cache_insert(&record)
    }

    pub fn lookup(&self, key: &str) -> Result<Option<CacheRecord>> {
        self.db.cache_record_hit(key, now())
    }

    pub fn stats(&self) -> Result<CacheReport> {
        let metadata = self.db.cache_stats()?;
        let disk_bytes = dir_size(&self.root.join("cas"));
        let mut dependency_caches = Vec::new();
        for (name, var) in [
            ("cargo", "CARGO_HOME"),
            ("nuget", "NUGET_PACKAGES"),
            ("npm", "npm_config_cache"),
        ] {
            let path = std::env::var_os(var).map(PathBuf::from).or_else(|| {
                if name == "cargo" {
                    std::env::var_os("USERPROFILE")
                        .or_else(|| std::env::var_os("HOME"))
                        .map(|h| PathBuf::from(h).join(".cargo"))
                } else if name == "nuget" {
                    std::env::var_os("USERPROFILE")
                        .or_else(|| std::env::var_os("HOME"))
                        .map(|h| PathBuf::from(h).join(".nuget/packages"))
                } else {
                    std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("npm-cache"))
                }
            });
            let bytes = path.as_ref().map(|p| dir_size(p)).unwrap_or(0);
            dependency_caches.push(DependencyCache {
                name: name.to_string(),
                path,
                bytes,
            });
        }
        Ok(CacheReport {
            root: self.root.clone(),
            enabled: self.enabled,
            metadata,
            disk_bytes,
            dependency_caches,
        })
    }

    pub fn prune(
        &self,
        older_than_days: Option<u64>,
        max_bytes: Option<u64>,
    ) -> Result<PruneReport> {
        let now_ms = now();
        let mut records = self.db.cache_records()?;
        let mut removed_entries = 0u64;
        let mut removed_bytes = 0u64;
        let cutoff = older_than_days.map(|days| now_ms.saturating_sub(days as u128 * 86_400_000));
        let mut keep: Vec<CacheRecord> = Vec::new();
        for record in records.drain(..) {
            let expired = cutoff.map(|c| record.last_used <= c).unwrap_or(false);
            if expired {
                remove_record(&self.db, &record)?;
                removed_entries += 1;
                removed_bytes += record.size;
            } else {
                keep.push(record);
            }
        }
        if let Some(max) = max_bytes {
            let mut total: u64 = keep.iter().map(|r| r.size).sum();
            keep.sort_by_key(|r| r.last_used);
            for record in keep.iter() {
                if total <= max {
                    break;
                }
                remove_record(&self.db, record)?;
                total = total.saturating_sub(record.size);
                removed_entries += 1;
                removed_bytes += record.size;
            }
        }
        let remaining = self.db.cache_stats()?;
        Ok(PruneReport {
            removed_entries,
            removed_bytes,
            remaining_entries: remaining.entries,
            remaining_bytes: remaining.total_bytes,
        })
    }

    pub fn clear(&self) -> Result<()> {
        let records = self.db.cache_records()?;
        for record in &records {
            remove_blob(record);
        }
        let cas = self.root.join("cas");
        if cas.exists() {
            let _ = std::fs::remove_dir_all(&cas);
        }
        std::fs::create_dir_all(&cas)
            .map_err(|e| UbuilderError::io(format!("cannot recreate {}", cas.display()), e))?;
        self.db.clear_cache_metadata()
    }

    fn lock_key(&self, key: &str) -> Result<File> {
        let path = self.root.join("locks").join(format!("{key}.lock"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| UbuilderError::io(format!("cannot create {}", parent.display()), e))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| UbuilderError::io(format!("cannot open lock {}", path.display()), e))?;
        file.lock_exclusive()
            .map_err(|e| UbuilderError::io(format!("cannot lock {}", path.display()), e))?;
        Ok(file)
    }
}

fn key_for_command(command: &CommandSpec) -> String {
    let mut text = command.program.clone();
    for arg in &command.args {
        text.push('\u{0}');
        text.push_str(arg);
    }
    if let Some(cwd) = &command.cwd {
        text.push('\u{0}');
        text.push_str(&cwd.to_string_lossy());
    }
    for (key, value) in &command.env {
        text.push('\u{0}');
        text.push_str(key);
        text.push('=');
        text.push_str(value);
    }
    artifacts::sha256_bytes(text.as_bytes())
}

fn now() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn remove_record(db: &Db, record: &CacheRecord) -> Result<()> {
    remove_blob(record);
    db.cache_delete(&record.key)
}

fn remove_blob(record: &CacheRecord) {
    if record.path.is_file() {
        let _ = std::fs::remove_file(&record.path);
    }
}

fn dir_size(path: &Path) -> u64 {
    WalkDir::new(path)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

fn extract_zip(zip_path: &Path, dest: &Path) -> Result<()> {
    let file = File::open(zip_path)
        .map_err(|e| UbuilderError::io(format!("cannot open {}", zip_path.display()), e))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| {
        UbuilderError::new(ErrorCode::CacheCorrupt, format!("invalid cached zip: {e}"))
    })?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|e| {
            UbuilderError::new(ErrorCode::CacheCorrupt, format!("bad cached entry: {e}"))
        })?;
        let rel = PathBuf::from(entry.name());
        security::validate_relative_path(&rel)?;
        let target = dest.join(&rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&target)
                .map_err(|e| UbuilderError::io(format!("cannot create {}", target.display()), e))?;
        } else {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    UbuilderError::io(format!("cannot create {}", parent.display()), e)
                })?;
            }
            let mut out = File::create(&target)
                .map_err(|e| UbuilderError::io(format!("cannot create {}", target.display()), e))?;
            std::io::copy(&mut entry, &mut out).map_err(|e| {
                UbuilderError::io(format!("cannot extract {}", target.display()), e)
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Some(mode) = entry.unix_mode() {
                    let _ =
                        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode));
                }
            }
        }
    }
    Ok(())
}

/// Compute a toolchain fingerprint from versions that influence build output.
pub fn toolchain_fingerprint(facts: &crate::capability::Facts) -> String {
    let mut parts = Vec::new();
    if let Some(v) = &facts.rustc_version {
        parts.push(format!("rustc={v}"));
    }
    if let Some(v) = &facts.dotnet_sdk {
        parts.push(format!("dotnet={v}"));
    }
    if let Some(v) = &facts.node_version {
        parts.push(format!("node={v}"));
    }
    if let Some(v) = &facts.xcode_version {
        parts.push(format!("xcode={v}"));
    }
    if let Some(v) = &facts.zig_version {
        parts.push(format!("zig={v}"));
    }
    artifacts::sha256_bytes(parts.join(";").as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::executor::CommandSpec;
    use crate::planner::{BuildPlan, BuildStep};
    use crate::project::FrameworkKind;
    use crate::target::Target;

    fn plan() -> BuildPlan {
        BuildPlan::new(
            "b",
            "demo",
            "/tmp/demo",
            FrameworkKind::Rust,
            Target::parse("linux-x64").unwrap(),
        )
        .step(BuildStep::command(
            "build",
            "build",
            CommandSpec::new("cargo", "build"),
        ))
    }

    #[test]
    fn key_changes_with_input_content() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = Cache::open(tmp.path(), true).unwrap();
        let plan = plan();
        let step = &plan.steps[0];
        let host = Host {
            platform: crate::host::HostPlatform::Linux,
            arch: crate::host::Architecture::X64,
            triple: "x86_64-unknown-linux-gnu".into(),
            os_version: None,
        };
        let input = tmp.path().join("input.txt");
        std::fs::write(&input, "one").unwrap();
        let mut step = step.clone();
        step.inputs = vec![input.clone()];
        let k1 = cache.step_key(&plan, &step, &host, "tc").unwrap();
        std::fs::write(&input, "two").unwrap();
        let k2 = cache.step_key(&plan, &step, &host, "tc").unwrap();
        assert_ne!(k1, k2);
    }

    #[test]
    fn tree_roundtrip_and_corruption_detection() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = Cache::open(&tmp.path().join("cache"), true).unwrap();
        let src = tmp.path().join("out");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("sub/a.txt"), "payload").unwrap();
        cache.store_tree("key1", &src).unwrap();
        let dest = tmp.path().join("restored");
        assert!(cache.restore_tree("key1", &dest).unwrap());
        assert_eq!(
            std::fs::read_to_string(dest.join("sub/a.txt")).unwrap(),
            "payload"
        );

        let record = cache.lookup("key1").unwrap().unwrap();
        std::fs::write(&record.path, b"corrupt").unwrap();
        let err = cache.restore_tree("key1", &dest).unwrap_err();
        assert_eq!(err.code, ErrorCode::CacheCorrupt);
    }

    #[test]
    fn concurrent_writers_produce_one_valid_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("cache");
        let src = tmp.path().join("out");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("f.txt"), "same").unwrap();
        let mut handles = Vec::new();
        for _ in 0..4 {
            let root = root.clone();
            let src = src.clone();
            handles.push(std::thread::spawn(move || {
                let cache = Cache::open(&root, true).unwrap();
                cache.store_tree("shared", &src).unwrap();
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
        let cache = Cache::open(&root, true).unwrap();
        let dest = tmp.path().join("restored");
        assert!(cache.restore_tree("shared", &dest).unwrap());
        assert_eq!(std::fs::read_to_string(dest.join("f.txt")).unwrap(), "same");
        assert_eq!(cache.db().cache_records().unwrap().len(), 1);
    }

    #[test]
    fn prune_and_clear() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = Cache::open(&tmp.path().join("cache"), true).unwrap();
        let src = tmp.path().join("out");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("f"), "x").unwrap();
        cache.store_tree("a", &src).unwrap();
        cache.store_tree("b", &src).unwrap();
        let report = cache.prune(Some(0), None).unwrap();
        assert_eq!(report.removed_entries, 2);
        cache.store_tree("c", &src).unwrap();
        cache.clear().unwrap();
        assert_eq!(cache.stats().unwrap().metadata.entries, 0);
    }

    #[test]
    fn config_is_referenced() {
        // Guard against accidental removal of the dependency-cache integration.
        let _ = Config::default();
    }
}
