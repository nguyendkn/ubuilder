//! SQLite-backed build job, step, artifact and cache metadata.
//!
//! WAL mode and a busy timeout make concurrent reader/writer processes safe;
//! state transitions that matter (finishing a job) run inside a transaction.

use crate::error::{ErrorCode, Result, UbuilderError};
use crate::pipeline::state::{validate_transition, RunState, StateEntity};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// Bump when a migration is added. A database written by a newer `ubuilder`
/// is rejected before any DDL so an older binary cannot silently corrupt state.
pub const SCHEMA_VERSION: u32 = 2;

pub struct Db {
    conn: Mutex<Connection>,
    path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConcurrencyLease {
    pub group: String,
    pub owner: String,
    pub slot: i64,
    pub token: String,
    pub generation: i64,
    pub lease_until: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRecord {
    pub id: String,
    pub project: String,
    pub framework: String,
    pub target: String,
    pub host: String,
    pub status: String,
    pub started_at: u128,
    pub finished_at: Option<u128>,
    pub manifest_path: Option<PathBuf>,
    pub log_path: Option<PathBuf>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepRecord {
    pub job_id: String,
    pub step_id: String,
    pub name: String,
    pub status: String,
    pub exit_code: Option<i32>,
    pub started_at: Option<u128>,
    pub finished_at: Option<u128>,
    pub log_path: Option<PathBuf>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRecord {
    pub id: Option<i64>,
    pub job_id: String,
    pub target: String,
    pub framework: String,
    pub kind: String,
    pub format: String,
    pub path: PathBuf,
    pub sha256: String,
    pub size: u64,
    pub validation: Option<String>,
    pub created_at: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheRecord {
    pub key: String,
    pub kind: String,
    pub path: PathBuf,
    pub size: u64,
    pub created_at: u128,
    pub last_used: u128,
    pub hits: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheStats {
    pub entries: u64,
    pub total_bytes: u64,
    pub hits: u64,
    pub miss: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineRunRecord {
    pub id: String,
    pub workflow: String,
    pub source_path: PathBuf,
    pub source_digest: String,
    pub status: String,
    pub created_at: u128,
    pub started_at: Option<u128>,
    pub finished_at: Option<u128>,
    pub host: String,
    pub inputs_json: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineJobRecord {
    pub run_id: String,
    pub id: String,
    pub base_id: String,
    pub name: String,
    pub matrix_json: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub started_at: Option<u128>,
    pub finished_at: Option<u128>,
    pub error: Option<String>,
    pub outputs_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineStepRecord {
    pub run_id: String,
    pub job_id: String,
    pub step_id: String,
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub exit_code: Option<i32>,
    pub started_at: Option<u128>,
    pub finished_at: Option<u128>,
    pub log_path: Option<PathBuf>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineArtifactRecord {
    pub id: Option<i64>,
    pub run_id: String,
    pub job_id: String,
    pub name: String,
    pub path: PathBuf,
    pub sha256: String,
    pub size: u64,
    pub created_at: u128,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| UbuilderError::io(format!("cannot create {}", parent.display()), e))?;
        }
        let conn = Connection::open(path)
            .map_err(|e| db_error(format!("cannot open {}", path.display()), e))?;
        configure(&conn)?;
        let db = Self {
            conn: Mutex::new(conn),
            path: path.to_path_buf(),
        };
        db.migrate()?;
        Ok(db)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(|e| db_error("in-memory db", e))?;
        configure(&conn)?;
        let db = Self {
            conn: Mutex::new(conn),
            path: PathBuf::from(":memory:"),
        };
        db.migrate()?;
        Ok(db)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn schema_version(&self) -> Result<u32> {
        let conn = self.lock();
        read_schema_version(&conn)
    }

    fn migrate(&self) -> Result<()> {
        let mut conn = self.lock();
        let existing = read_schema_version(&conn)?;
        if existing > SCHEMA_VERSION {
            return Err(UbuilderError::new(
                ErrorCode::SchemaUnsupported,
                format!(
                    "database schema version {existing} is newer than this ubuilder supports ({SCHEMA_VERSION})"
                ),
            )
            .remediation("Upgrade ubuilder before using this data directory")
            .details(serde_json::json!({
                "database": self.path,
                "found_version": existing,
                "supported_version": SCHEMA_VERSION,
            })));
        }
        let has_tables: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name <> 'schema_migrations')",
                [],
                |row| row.get(0),
            )
            .map_err(|e| db_error("inspect schema", e))?;
        if has_tables && existing < SCHEMA_VERSION && self.path != Path::new(":memory:") {
            backup_before_migration(&conn, &self.path, existing)?;
        }
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| db_error("begin schema migration", e))?;
        tx.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS jobs (
                id TEXT PRIMARY KEY,
                project TEXT NOT NULL,
                framework TEXT NOT NULL,
                target TEXT NOT NULL,
                host TEXT NOT NULL,
                status TEXT NOT NULL,
                started_at INTEGER NOT NULL,
                finished_at INTEGER,
                manifest_path TEXT,
                log_path TEXT,
                error TEXT
            );
            CREATE TABLE IF NOT EXISTS steps (
                job_id TEXT NOT NULL,
                step_id TEXT NOT NULL,
                name TEXT NOT NULL,
                status TEXT NOT NULL,
                exit_code INTEGER,
                started_at INTEGER,
                finished_at INTEGER,
                log_path TEXT,
                error TEXT,
                PRIMARY KEY (job_id, step_id)
            );
            CREATE TABLE IF NOT EXISTS artifacts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                job_id TEXT NOT NULL,
                target TEXT NOT NULL,
                framework TEXT NOT NULL,
                kind TEXT NOT NULL,
                format TEXT NOT NULL,
                path TEXT NOT NULL,
                sha256 TEXT NOT NULL,
                size INTEGER NOT NULL,
                validation TEXT,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_artifacts_target ON artifacts(target);
            CREATE INDEX IF NOT EXISTS idx_artifacts_sha ON artifacts(sha256);
            CREATE TABLE IF NOT EXISTS cache_entries (
                key TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                path TEXT NOT NULL,
                size INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                last_used INTEGER NOT NULL,
                hits INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS cache_stats (
                name TEXT PRIMARY KEY,
                value INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS pipeline_runs (
                id TEXT PRIMARY KEY,
                workflow TEXT NOT NULL,
                source_path TEXT NOT NULL,
                source_digest TEXT NOT NULL,
                status TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                started_at INTEGER,
                finished_at INTEGER,
                host TEXT NOT NULL,
                inputs_json TEXT NOT NULL DEFAULT '{}',
                error TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_pipeline_runs_created ON pipeline_runs(created_at);
            CREATE TABLE IF NOT EXISTS pipeline_jobs (
                run_id TEXT NOT NULL,
                id TEXT NOT NULL,
                base_id TEXT NOT NULL,
                name TEXT NOT NULL,
                matrix_json TEXT NOT NULL DEFAULT '{}',
                status TEXT NOT NULL,
                conclusion TEXT,
                started_at INTEGER,
                finished_at INTEGER,
                error TEXT,
                outputs_json TEXT NOT NULL DEFAULT '{}',
                PRIMARY KEY (run_id, id)
            );
            CREATE TABLE IF NOT EXISTS pipeline_steps (
                run_id TEXT NOT NULL,
                job_id TEXT NOT NULL,
                step_id TEXT NOT NULL,
                name TEXT NOT NULL,
                status TEXT NOT NULL,
                conclusion TEXT,
                exit_code INTEGER,
                started_at INTEGER,
                finished_at INTEGER,
                log_path TEXT,
                error TEXT,
                PRIMARY KEY (run_id, job_id, step_id)
            );
            CREATE TABLE IF NOT EXISTS pipeline_artifacts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id TEXT NOT NULL,
                job_id TEXT NOT NULL,
                name TEXT NOT NULL,
                path TEXT NOT NULL,
                sha256 TEXT NOT NULL,
                size INTEGER NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_pipeline_artifacts_run ON pipeline_artifacts(run_id, name);
            CREATE TABLE IF NOT EXISTS pipeline_locks (
                group_name TEXT PRIMARY KEY,
                owner TEXT NOT NULL,
                lease_until INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS pipeline_concurrency_slots (
                group_name TEXT NOT NULL,
                slot INTEGER NOT NULL,
                owner TEXT NOT NULL,
                lease_until INTEGER NOT NULL,
                token TEXT NOT NULL DEFAULT '',
                generation INTEGER NOT NULL DEFAULT 1,
                PRIMARY KEY (group_name, slot)
            );
            CREATE INDEX IF NOT EXISTS idx_concurrency_owner
                ON pipeline_concurrency_slots (group_name, owner);
            CREATE TABLE IF NOT EXISTS pipeline_concurrency_generations (
                group_name TEXT PRIMARY KEY,
                generation INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS artifact_pins (
                sha256 TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at INTEGER NOT NULL
            );
            "#,
        )
        .map_err(|e| db_error("migrate schema", e))?;
        add_column_if_missing(
            &tx,
            "pipeline_concurrency_slots",
            "token",
            "TEXT NOT NULL DEFAULT ''",
        )?;
        add_column_if_missing(
            &tx,
            "pipeline_concurrency_slots",
            "generation",
            "INTEGER NOT NULL DEFAULT 1",
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
            params![SCHEMA_VERSION as i64, crate::telemetry::now_ms() as i64],
        )
        .map_err(|e| db_error("record schema version", e))?;
        tx.commit()
            .map_err(|e| db_error("commit schema migration", e))?;
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn upsert_job(&self, job: &JobRecord) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            r#"INSERT INTO jobs (id, project, framework, target, host, status, started_at, finished_at, manifest_path, log_path, error)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
               ON CONFLICT(id) DO UPDATE SET status=excluded.status, finished_at=excluded.finished_at,
                 manifest_path=excluded.manifest_path, log_path=excluded.log_path, error=excluded.error"#,
            params![
                job.id,
                job.project,
                job.framework,
                job.target,
                job.host,
                job.status,
                job.started_at as i64,
                job.finished_at.map(|v| v as i64),
                job.manifest_path.as_ref().map(|p| p.to_string_lossy().to_string()),
                job.log_path.as_ref().map(|p| p.to_string_lossy().to_string()),
                job.error,
            ],
        )
        .map_err(|e| db_error("upsert job", e))?;
        Ok(())
    }

    pub fn finish_job(
        &self,
        job_id: &str,
        status: &str,
        error: Option<&str>,
        manifest: Option<&Path>,
        finished_at: u128,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn
            .transaction()
            .map_err(|e| db_error("begin job transaction", e))?;
        tx.execute(
            "UPDATE jobs SET status=?1, error=?2, manifest_path=?3, finished_at=?4 WHERE id=?5",
            params![
                status,
                error,
                manifest.map(|p| p.to_string_lossy().to_string()),
                finished_at as i64,
                job_id
            ],
        )
        .map_err(|e| db_error("finish job", e))?;
        tx.commit().map_err(|e| db_error("commit job", e))?;
        Ok(())
    }

    pub fn upsert_step(&self, step: &StepRecord) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            r#"INSERT INTO steps (job_id, step_id, name, status, exit_code, started_at, finished_at, log_path, error)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
               ON CONFLICT(job_id, step_id) DO UPDATE SET status=excluded.status,
                 exit_code=excluded.exit_code, started_at=COALESCE(excluded.started_at, steps.started_at),
                 finished_at=excluded.finished_at, log_path=excluded.log_path, error=excluded.error"#,
            params![
                step.job_id,
                step.step_id,
                step.name,
                step.status,
                step.exit_code,
                step.started_at.map(|v| v as i64),
                step.finished_at.map(|v| v as i64),
                step.log_path.as_ref().map(|p| p.to_string_lossy().to_string()),
                step.error,
            ],
        )
        .map_err(|e| db_error("upsert step", e))?;
        Ok(())
    }

    pub fn add_artifact(&self, artifact: &ArtifactRecord) -> Result<i64> {
        let conn = self.lock();
        conn.execute(
            r#"INSERT INTO artifacts (job_id, target, framework, kind, format, path, sha256, size, validation, created_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"#,
            params![
                artifact.job_id,
                artifact.target,
                artifact.framework,
                artifact.kind,
                artifact.format,
                artifact.path.to_string_lossy().to_string(),
                artifact.sha256,
                artifact.size as i64,
                artifact.validation,
                artifact.created_at as i64,
            ],
        )
        .map_err(|e| db_error("insert artifact", e))?;
        Ok(conn.last_insert_rowid())
    }

    pub fn list_artifacts(&self, target: Option<&str>) -> Result<Vec<ArtifactRecord>> {
        let conn = self.lock();
        let mut stmt = match target {
            Some(_) => conn
                .prepare("SELECT id, job_id, target, framework, kind, format, path, sha256, size, validation, created_at FROM artifacts WHERE target=?1 ORDER BY created_at DESC")
                .map_err(|e| db_error("prepare artifact query", e))?,
            None => conn
                .prepare("SELECT id, job_id, target, framework, kind, format, path, sha256, size, validation, created_at FROM artifacts ORDER BY created_at DESC")
                .map_err(|e| db_error("prepare artifact query", e))?,
        };
        let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<ArtifactRecord> {
            Ok(ArtifactRecord {
                id: Some(row.get(0)?),
                job_id: row.get(1)?,
                target: row.get(2)?,
                framework: row.get(3)?,
                kind: row.get(4)?,
                format: row.get(5)?,
                path: PathBuf::from(row.get::<_, String>(6)?),
                sha256: row.get(7)?,
                size: row.get::<_, i64>(8)? as u64,
                validation: row.get(9)?,
                created_at: row.get::<_, i64>(10)? as u128,
            })
        };
        let rows = match target {
            Some(target) => stmt
                .query_map(params![target], map_row)
                .map_err(|e| db_error("query artifacts", e))?,
            None => stmt
                .query_map([], map_row)
                .map_err(|e| db_error("query artifacts", e))?,
        };
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| db_error("read artifacts", e))
    }

    pub fn artifact_by_sha(&self, sha: &str) -> Result<Option<ArtifactRecord>> {
        Ok(self
            .list_artifacts(None)?
            .into_iter()
            .find(|a| a.sha256 == sha))
    }

    pub fn recent_jobs(&self, limit: usize) -> Result<Vec<JobRecord>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT id, project, framework, target, host, status, started_at, finished_at, manifest_path, log_path, error FROM jobs ORDER BY started_at DESC LIMIT ?1")
            .map_err(|e| db_error("prepare job query", e))?;
        let rows = stmt
            .query_map(params![limit as i64], |row| {
                Ok(JobRecord {
                    id: row.get(0)?,
                    project: row.get(1)?,
                    framework: row.get(2)?,
                    target: row.get(3)?,
                    host: row.get(4)?,
                    status: row.get(5)?,
                    started_at: row.get::<_, i64>(6)? as u128,
                    finished_at: row.get::<_, Option<i64>>(7)?.map(|v| v as u128),
                    manifest_path: row.get::<_, Option<String>>(8)?.map(PathBuf::from),
                    log_path: row.get::<_, Option<String>>(9)?.map(PathBuf::from),
                    error: row.get(10)?,
                })
            })
            .map_err(|e| db_error("query jobs", e))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| db_error("read jobs", e))
    }

    pub fn cache_record_hit(&self, key: &str, now: u128) -> Result<Option<CacheRecord>> {
        let conn = self.lock();
        let record = conn
            .query_row(
                "SELECT key, kind, path, size, created_at, last_used, hits FROM cache_entries WHERE key=?1",
                params![key],
                |row| {
                    Ok(CacheRecord {
                        key: row.get(0)?,
                        kind: row.get(1)?,
                        path: PathBuf::from(row.get::<_, String>(2)?),
                        size: row.get::<_, i64>(3)? as u64,
                        created_at: row.get::<_, i64>(4)? as u128,
                        last_used: row.get::<_, i64>(5)? as u128,
                        hits: row.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(|e| db_error("cache lookup", e))?;
        if record.is_some() {
            conn.execute(
                "UPDATE cache_entries SET hits=hits+1, last_used=?1 WHERE key=?2",
                params![now as i64, key],
            )
            .map_err(|e| db_error("cache touch", e))?;
            bump_stat(&conn, "hits", 1)?;
        } else {
            bump_stat(&conn, "miss", 1)?;
        }
        Ok(record)
    }

    pub fn cache_insert(&self, record: &CacheRecord) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            r#"INSERT INTO cache_entries (key, kind, path, size, created_at, last_used, hits)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
               ON CONFLICT(key) DO UPDATE SET kind=excluded.kind, path=excluded.path, size=excluded.size, last_used=excluded.last_used"#,
            params![
                record.key,
                record.kind,
                record.path.to_string_lossy().to_string(),
                record.size as i64,
                record.created_at as i64,
                record.last_used as i64,
                record.hits,
            ],
        )
        .map_err(|e| db_error("cache insert", e))?;
        Ok(())
    }

    pub fn cache_records(&self) -> Result<Vec<CacheRecord>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT key, kind, path, size, created_at, last_used, hits FROM cache_entries ORDER BY created_at")
            .map_err(|e| db_error("prepare cache list", e))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(CacheRecord {
                    key: row.get(0)?,
                    kind: row.get(1)?,
                    path: PathBuf::from(row.get::<_, String>(2)?),
                    size: row.get::<_, i64>(3)? as u64,
                    created_at: row.get::<_, i64>(4)? as u128,
                    last_used: row.get::<_, i64>(5)? as u128,
                    hits: row.get(6)?,
                })
            })
            .map_err(|e| db_error("query cache", e))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| db_error("read cache", e))
    }

    pub fn cache_delete(&self, key: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute("DELETE FROM cache_entries WHERE key=?1", params![key])
            .map_err(|e| db_error("cache delete", e))?;
        Ok(())
    }

    pub fn cache_stats(&self) -> Result<CacheStats> {
        let conn = self.lock();
        let entries: i64 = conn
            .query_row("SELECT COUNT(*) FROM cache_entries", [], |r| r.get(0))
            .map_err(|e| db_error("cache stats", e))?;
        let total: i64 = conn
            .query_row("SELECT COALESCE(SUM(size),0) FROM cache_entries", [], |r| {
                r.get(0)
            })
            .map_err(|e| db_error("cache stats", e))?;
        let hits = read_stat(&conn, "hits")?;
        let miss = read_stat(&conn, "miss")?;
        Ok(CacheStats {
            entries: entries as u64,
            total_bytes: total as u64,
            hits,
            miss,
        })
    }

    pub fn clear_cache_metadata(&self) -> Result<()> {
        let conn = self.lock();
        conn.execute_batch("DELETE FROM cache_entries; DELETE FROM cache_stats;")
            .map_err(|e| db_error("clear cache metadata", e))?;
        Ok(())
    }

    pub fn insert_pipeline_run(&self, run: &PipelineRunRecord) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            r#"INSERT INTO pipeline_runs
               (id, workflow, source_path, source_digest, status, created_at, started_at, finished_at, host, inputs_json, error)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"#,
            params![
                run.id,
                run.workflow,
                run.source_path.to_string_lossy().to_string(),
                run.source_digest,
                run.status,
                run.created_at as i64,
                run.started_at.map(|v| v as i64),
                run.finished_at.map(|v| v as i64),
                run.host,
                run.inputs_json,
                run.error,
            ],
        )
        .map_err(|e| db_error("insert pipeline run", e))?;
        Ok(())
    }

    pub fn update_pipeline_run(
        &self,
        id: &str,
        status: &str,
        error: Option<&str>,
        finished_at: Option<u128>,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn
            .transaction()
            .map_err(|e| db_error("begin run transition", e))?;
        let current: Option<String> = tx
            .query_row(
                "SELECT status FROM pipeline_runs WHERE id=?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| db_error("read run state", e))?;
        let to = RunState::parse(status).ok_or_else(|| {
            UbuilderError::new(
                ErrorCode::StateInvalid,
                format!("unknown run state '{status}'"),
            )
        })?;
        if let Some(current) = current {
            validate_transition(StateEntity::Run, &current, to)?;
        }
        tx.execute(
            "UPDATE pipeline_runs SET status=?1, error=COALESCE(?2, error), finished_at=COALESCE(?3, finished_at) WHERE id=?4",
            params![status, error, finished_at.map(|v| v as i64), id],
        )
        .map_err(|e| db_error("update pipeline run", e))?;
        tx.commit()
            .map_err(|e| db_error("commit run transition", e))?;
        Ok(())
    }

    pub fn all_pipeline_runs(&self) -> Result<Vec<PipelineRunRecord>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, workflow, source_path, source_digest, status, created_at, started_at, finished_at, host, inputs_json, error FROM pipeline_runs ORDER BY created_at DESC",
            )
            .map_err(|e| db_error("prepare all pipeline runs", e))?;
        let rows = stmt
            .query_map([], pipeline_run_row)
            .map_err(|e| db_error("query all pipeline runs", e))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| db_error("read all pipeline runs", e))
    }

    /// Delete one run and its jobs, steps and artifact references atomically.
    pub fn delete_pipeline_run(&self, id: &str) -> Result<bool> {
        let mut conn = self.lock();
        let tx = conn
            .transaction()
            .map_err(|e| db_error("begin run deletion", e))?;
        tx.execute("DELETE FROM pipeline_steps WHERE run_id=?1", params![id])
            .map_err(|e| db_error("delete run steps", e))?;
        tx.execute("DELETE FROM pipeline_jobs WHERE run_id=?1", params![id])
            .map_err(|e| db_error("delete run jobs", e))?;
        tx.execute(
            "DELETE FROM pipeline_artifacts WHERE run_id=?1",
            params![id],
        )
        .map_err(|e| db_error("delete run artifacts", e))?;
        let removed = tx
            .execute("DELETE FROM pipeline_runs WHERE id=?1", params![id])
            .map_err(|e| db_error("delete run", e))?;
        tx.commit()
            .map_err(|e| db_error("commit run deletion", e))?;
        Ok(removed > 0)
    }

    /// Map object SHA-256 to (reference count, bytes) across retained artifacts.
    pub fn pipeline_artifact_references(
        &self,
    ) -> Result<std::collections::BTreeMap<String, (u64, u64)>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT sha256, COUNT(*), MAX(size) FROM pipeline_artifacts GROUP BY sha256")
            .map_err(|e| db_error("prepare artifact references", e))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    (row.get::<_, i64>(1)? as u64, row.get::<_, i64>(2)? as u64),
                ))
            })
            .map_err(|e| db_error("query artifact references", e))?;
        rows.collect::<rusqlite::Result<std::collections::BTreeMap<_, _>>>()
            .map_err(|e| db_error("read artifact references", e))
    }

    pub fn pin_artifact(&self, sha256: &str, name: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT OR REPLACE INTO artifact_pins (sha256, name, created_at) VALUES (?1, ?2, ?3)",
            params![sha256, name, crate::telemetry::now_ms() as i64],
        )
        .map_err(|e| db_error("pin artifact", e))?;
        Ok(())
    }

    pub fn unpin_artifact(&self, sha256: &str) -> Result<bool> {
        let conn = self.lock();
        let removed = conn
            .execute("DELETE FROM artifact_pins WHERE sha256=?1", params![sha256])
            .map_err(|e| db_error("unpin artifact", e))?;
        Ok(removed > 0)
    }

    pub fn pinned_artifacts(&self) -> Result<Vec<(String, String)>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT sha256, name FROM artifact_pins ORDER BY created_at")
            .map_err(|e| db_error("prepare pins", e))?;
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|e| db_error("query pins", e))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| db_error("read pins", e))
    }

    pub fn pipeline_run(&self, id: &str) -> Result<Option<PipelineRunRecord>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT id, workflow, source_path, source_digest, status, created_at, started_at, finished_at, host, inputs_json, error FROM pipeline_runs WHERE id=?1",
            params![id],
            pipeline_run_row,
        )
        .optional()
        .map_err(|e| db_error("read pipeline run", e))
    }

    pub fn pipeline_runs(&self, limit: usize) -> Result<Vec<PipelineRunRecord>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, workflow, source_path, source_digest, status, created_at, started_at, finished_at, host, inputs_json, error FROM pipeline_runs ORDER BY created_at DESC LIMIT ?1",
            )
            .map_err(|e| db_error("prepare pipeline runs", e))?;
        let rows = stmt
            .query_map(params![limit as i64], pipeline_run_row)
            .map_err(|e| db_error("query pipeline runs", e))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| db_error("read pipeline runs", e))
    }

    pub fn recover_stale_pipeline_runs(&self, stale_before: u128) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id FROM pipeline_runs WHERE status='running' AND COALESCE(started_at, created_at) < ?1",
            )
            .map_err(|e| db_error("prepare stale runs", e))?;
        let ids = stmt
            .query_map(params![stale_before as i64], |row| row.get::<_, String>(0))
            .map_err(|e| db_error("query stale runs", e))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| db_error("read stale runs", e))?;
        for id in &ids {
            conn.execute(
                "UPDATE pipeline_runs SET status='failed', error='run interrupted (stale running state; side effects of in-flight steps are unknown)', finished_at=?1 WHERE id=?2",
                params![crate::telemetry::now_ms() as i64, id],
            )
            .map_err(|e| db_error("recover stale run", e))?;
            conn.execute(
                "UPDATE pipeline_jobs SET status=CASE WHEN status='running' THEN 'failed' ELSE status END, conclusion=CASE WHEN conclusion IS NULL THEN 'failed' ELSE conclusion END, error=COALESCE(error, 'run interrupted') WHERE run_id=?1",
                params![id],
            )
            .map_err(|e| db_error("recover stale jobs", e))?;
            // The process that owned a slot is gone; do not make the next run
            // wait for the full lease TTL after an explicit recovery.
            conn.execute(
                "DELETE FROM pipeline_concurrency_slots WHERE owner LIKE ?1",
                params![format!("{id}/%")],
            )
            .map_err(|e| db_error("release stale concurrency leases", e))?;
        }
        Ok(ids)
    }

    pub fn upsert_pipeline_job(&self, job: &PipelineJobRecord) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn
            .transaction()
            .map_err(|e| db_error("begin job transition", e))?;
        let current: Option<String> = tx
            .query_row(
                "SELECT status FROM pipeline_jobs WHERE run_id=?1 AND id=?2",
                params![job.run_id, job.id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| db_error("read job state", e))?;
        let to = RunState::parse(&job.status).ok_or_else(|| {
            UbuilderError::new(
                ErrorCode::StateInvalid,
                format!("unknown job state '{}'", job.status),
            )
        })?;
        if let Some(current) = current {
            validate_transition(StateEntity::Job, &current, to)?;
        }
        tx.execute(
            r#"INSERT INTO pipeline_jobs
               (run_id, id, base_id, name, matrix_json, status, conclusion, started_at, finished_at, error, outputs_json)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
               ON CONFLICT(run_id, id) DO UPDATE SET status=excluded.status, conclusion=excluded.conclusion,
                 started_at=COALESCE(excluded.started_at, pipeline_jobs.started_at),
                 finished_at=excluded.finished_at, error=excluded.error, outputs_json=excluded.outputs_json"#,
            params![
                job.run_id,
                job.id,
                job.base_id,
                job.name,
                job.matrix_json,
                job.status,
                job.conclusion,
                job.started_at.map(|v| v as i64),
                job.finished_at.map(|v| v as i64),
                job.error,
                job.outputs_json,
            ],
        )
        .map_err(|e| db_error("upsert pipeline job", e))?;
        tx.commit()
            .map_err(|e| db_error("commit job transition", e))?;
        Ok(())
    }

    pub fn pipeline_jobs(&self, run_id: &str) -> Result<Vec<PipelineJobRecord>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT run_id, id, base_id, name, matrix_json, status, conclusion, started_at, finished_at, error, outputs_json FROM pipeline_jobs WHERE run_id=?1 ORDER BY rowid",
            )
            .map_err(|e| db_error("prepare pipeline jobs", e))?;
        let rows = stmt
            .query_map(params![run_id], |row| {
                Ok(PipelineJobRecord {
                    run_id: row.get(0)?,
                    id: row.get(1)?,
                    base_id: row.get(2)?,
                    name: row.get(3)?,
                    matrix_json: row.get(4)?,
                    status: row.get(5)?,
                    conclusion: row.get(6)?,
                    started_at: row.get::<_, Option<i64>>(7)?.map(|v| v as u128),
                    finished_at: row.get::<_, Option<i64>>(8)?.map(|v| v as u128),
                    error: row.get(9)?,
                    outputs_json: row.get(10)?,
                })
            })
            .map_err(|e| db_error("query pipeline jobs", e))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| db_error("read pipeline jobs", e))
    }

    pub fn upsert_pipeline_step(&self, step: &PipelineStepRecord) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn
            .transaction()
            .map_err(|e| db_error("begin step transition", e))?;
        let current: Option<String> = tx
            .query_row(
                "SELECT status FROM pipeline_steps WHERE run_id=?1 AND job_id=?2 AND step_id=?3",
                params![step.run_id, step.job_id, step.step_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| db_error("read step state", e))?;
        let to = RunState::parse(&step.status).ok_or_else(|| {
            UbuilderError::new(
                ErrorCode::StateInvalid,
                format!("unknown step state '{}'", step.status),
            )
        })?;
        if let Some(current) = current {
            validate_transition(StateEntity::Step, &current, to)?;
        }
        tx.execute(
            r#"INSERT INTO pipeline_steps
               (run_id, job_id, step_id, name, status, conclusion, exit_code, started_at, finished_at, log_path, error)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
               ON CONFLICT(run_id, job_id, step_id) DO UPDATE SET status=excluded.status,
                 conclusion=excluded.conclusion, exit_code=excluded.exit_code,
                 started_at=COALESCE(excluded.started_at, pipeline_steps.started_at),
                 finished_at=excluded.finished_at, log_path=excluded.log_path, error=excluded.error"#,
            params![
                step.run_id,
                step.job_id,
                step.step_id,
                step.name,
                step.status,
                step.conclusion,
                step.exit_code,
                step.started_at.map(|v| v as i64),
                step.finished_at.map(|v| v as i64),
                step.log_path.as_ref().map(|p| p.to_string_lossy().to_string()),
                step.error,
            ],
        )
        .map_err(|e| db_error("upsert pipeline step", e))?;
        tx.commit()
            .map_err(|e| db_error("commit step transition", e))?;
        Ok(())
    }

    pub fn pipeline_steps(&self, run_id: &str) -> Result<Vec<PipelineStepRecord>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT run_id, job_id, step_id, name, status, conclusion, exit_code, started_at, finished_at, log_path, error FROM pipeline_steps WHERE run_id=?1 ORDER BY rowid",
            )
            .map_err(|e| db_error("prepare pipeline steps", e))?;
        let rows = stmt
            .query_map(params![run_id], |row| {
                Ok(PipelineStepRecord {
                    run_id: row.get(0)?,
                    job_id: row.get(1)?,
                    step_id: row.get(2)?,
                    name: row.get(3)?,
                    status: row.get(4)?,
                    conclusion: row.get(5)?,
                    exit_code: row.get(6)?,
                    started_at: row.get::<_, Option<i64>>(7)?.map(|v| v as u128),
                    finished_at: row.get::<_, Option<i64>>(8)?.map(|v| v as u128),
                    log_path: row.get::<_, Option<String>>(9)?.map(PathBuf::from),
                    error: row.get(10)?,
                })
            })
            .map_err(|e| db_error("query pipeline steps", e))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| db_error("read pipeline steps", e))
    }

    pub fn add_pipeline_artifact(&self, artifact: &PipelineArtifactRecord) -> Result<i64> {
        let conn = self.lock();
        conn.execute(
            r#"INSERT INTO pipeline_artifacts (run_id, job_id, name, path, sha256, size, created_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"#,
            params![
                artifact.run_id,
                artifact.job_id,
                artifact.name,
                artifact.path.to_string_lossy().to_string(),
                artifact.sha256,
                artifact.size as i64,
                artifact.created_at as i64,
            ],
        )
        .map_err(|e| db_error("add pipeline artifact", e))?;
        Ok(conn.last_insert_rowid())
    }

    pub fn pipeline_artifacts(&self, run_id: &str) -> Result<Vec<PipelineArtifactRecord>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, run_id, job_id, name, path, sha256, size, created_at FROM pipeline_artifacts WHERE run_id=?1 ORDER BY id",
            )
            .map_err(|e| db_error("prepare pipeline artifacts", e))?;
        let rows = stmt
            .query_map(params![run_id], |row| {
                Ok(PipelineArtifactRecord {
                    id: Some(row.get(0)?),
                    run_id: row.get(1)?,
                    job_id: row.get(2)?,
                    name: row.get(3)?,
                    path: PathBuf::from(row.get::<_, String>(4)?),
                    sha256: row.get(5)?,
                    size: row.get::<_, i64>(6)? as u64,
                    created_at: row.get::<_, i64>(7)? as u128,
                })
            })
            .map_err(|e| db_error("query pipeline artifacts", e))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| db_error("read pipeline artifacts", e))
    }

    /// Acquire a lease. Expired leases are reclaimable. Returns false when a
    /// live owner holds the group.
    pub fn acquire_pipeline_lock(
        &self,
        group: &str,
        owner: &str,
        now: u128,
        lease_ms: u128,
    ) -> Result<bool> {
        let conn = self.lock();
        let changed = conn
            .execute(
                r#"INSERT INTO pipeline_locks (group_name, owner, lease_until) VALUES (?1, ?2, ?3)
                   ON CONFLICT(group_name) DO UPDATE SET owner=excluded.owner, lease_until=excluded.lease_until
                   WHERE pipeline_locks.lease_until < ?4"#,
                params![group, owner, (now + lease_ms) as i64, now as i64],
            )
            .map_err(|e| db_error("acquire pipeline lock", e))?;
        Ok(changed > 0)
    }

    /// Acquire one of `max` slots in a group and return a fenced lease.
    /// Expired leases are reclaimable. A stale owner cannot release or renew a
    /// lease after another process has acquired the same slot, because every
    /// renewal and release includes the lease token and generation.
    pub fn acquire_concurrency_lease(
        &self,
        group: &str,
        owner: &str,
        max: usize,
        now: u128,
        lease_ms: u128,
    ) -> Result<Option<ConcurrencyLease>> {
        let mut conn = self.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| db_error("begin concurrency slot", e))?;
        tx.execute(
            "DELETE FROM pipeline_concurrency_slots WHERE group_name=?1 AND lease_until < ?2",
            params![group, now as i64],
        )
        .map_err(|e| db_error("expire concurrency slots", e))?;
        let own: Option<(i64, String, i64)> = tx
            .query_row(
                "SELECT slot, token, generation FROM pipeline_concurrency_slots WHERE group_name=?1 AND owner=?2 LIMIT 1",
                params![group, owner],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|e| db_error("read own concurrency slot", e))?;
        if let Some((slot, token, generation)) = own {
            let token = if token.is_empty() {
                new_lease_token(group, owner, now)
            } else {
                token
            };
            let lease_until = now.saturating_add(lease_ms);
            tx.execute(
                "UPDATE pipeline_concurrency_slots SET lease_until=?1, token=?2, generation=?3 WHERE group_name=?4 AND slot=?5",
                params![lease_until as i64, token, generation, group, slot],
            )
            .map_err(|e| db_error("renew concurrency slot", e))?;
            tx.commit()
                .map_err(|e| db_error("commit concurrency slot", e))?;
            return Ok(Some(ConcurrencyLease {
                group: group.to_string(),
                owner: owner.to_string(),
                slot,
                token,
                generation,
                lease_until,
            }));
        }
        let used: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM pipeline_concurrency_slots WHERE group_name=?1",
                params![group],
                |row| row.get(0),
            )
            .map_err(|e| db_error("count concurrency slots", e))?;
        if used as usize >= max.max(1) {
            tx.commit()
                .map_err(|e| db_error("commit expired concurrency slots", e))?;
            return Ok(None);
        }
        let mut slot = 0i64;
        while tx
            .query_row(
                "SELECT 1 FROM pipeline_concurrency_slots WHERE group_name=?1 AND slot=?2",
                params![group, slot],
                |_| Ok(()),
            )
            .optional()
            .map_err(|e| db_error("find concurrency slot", e))?
            .is_some()
        {
            slot += 1;
        }
        let generation: i64 = tx
            .query_row(
                "INSERT INTO pipeline_concurrency_generations (group_name, generation) VALUES (?1, 1) ON CONFLICT(group_name) DO UPDATE SET generation=generation+1 RETURNING generation",
                params![group],
                |row| row.get(0),
            )
            .map_err(|e| db_error("next concurrency generation", e))?;
        let token = new_lease_token(group, owner, now);
        let lease_until = now.saturating_add(lease_ms);
        tx.execute(
            "INSERT INTO pipeline_concurrency_slots (group_name, slot, owner, lease_until, token, generation) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![group, slot, owner, lease_until as i64, token, generation],
        )
        .map_err(|e| db_error("insert concurrency slot", e))?;
        tx.commit()
            .map_err(|e| db_error("commit concurrency slot", e))?;
        Ok(Some(ConcurrencyLease {
            group: group.to_string(),
            owner: owner.to_string(),
            slot,
            token,
            generation,
            lease_until,
        }))
    }

    /// Refresh a lease. Returns false when the lease was lost.
    pub fn renew_concurrency_lease(
        &self,
        lease: &ConcurrencyLease,
        now: u128,
        lease_ms: u128,
    ) -> Result<bool> {
        let conn = self.lock();
        let changed = conn
            .execute(
                "UPDATE pipeline_concurrency_slots SET lease_until=?1 WHERE group_name=?2 AND slot=?3 AND owner=?4 AND token=?5 AND generation=?6",
                params![
                    now.saturating_add(lease_ms) as i64,
                    lease.group,
                    lease.slot,
                    lease.owner,
                    lease.token,
                    lease.generation
                ],
            )
            .map_err(|e| db_error("renew concurrency lease", e))?;
        Ok(changed == 1)
    }

    /// Release only the lease matching this token and generation.
    pub fn release_concurrency_lease(&self, lease: &ConcurrencyLease) -> Result<bool> {
        let conn = self.lock();
        let changed = conn
            .execute(
                "DELETE FROM pipeline_concurrency_slots WHERE group_name=?1 AND slot=?2 AND owner=?3 AND token=?4 AND generation=?5",
                params![
                    lease.group,
                    lease.slot,
                    lease.owner,
                    lease.token,
                    lease.generation
                ],
            )
            .map_err(|e| db_error("release concurrency slot", e))?;
        Ok(changed == 1)
    }

    /// Drop every expired slot. Used by crash recovery.
    pub fn expire_concurrency_leases(&self, now: u128) -> Result<usize> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM pipeline_concurrency_slots WHERE lease_until < ?1",
            params![now as i64],
        )
        .map_err(|e| db_error("expire concurrency slots", e))
    }

    /// Leases currently holding slots in a group.
    pub fn concurrency_leases(&self, group: &str) -> Result<Vec<ConcurrencyLease>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT group_name, slot, owner, token, generation, lease_until FROM pipeline_concurrency_slots WHERE group_name=?1 ORDER BY slot",
            )
            .map_err(|e| db_error("prepare concurrency leases", e))?;
        let rows = stmt
            .query_map(params![group], |row| {
                Ok(ConcurrencyLease {
                    group: row.get(0)?,
                    slot: row.get(1)?,
                    owner: row.get(2)?,
                    token: row.get(3)?,
                    generation: row.get(4)?,
                    lease_until: row.get::<_, i64>(5)? as u128,
                })
            })
            .map_err(|e| db_error("query concurrency leases", e))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| db_error("read concurrency leases", e))
    }

    /// Owners currently holding slots in a group.
    pub fn concurrency_owners(&self, group: &str) -> Result<Vec<String>> {
        Ok(self
            .concurrency_leases(group)?
            .into_iter()
            .map(|lease| lease.owner)
            .collect())
    }

    pub fn release_pipeline_lock(&self, group: &str, owner: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM pipeline_locks WHERE group_name=?1 AND owner=?2",
            params![group, owner],
        )
        .map_err(|e| db_error("release pipeline lock", e))?;
        Ok(())
    }
}

fn read_schema_version(conn: &Connection) -> Result<u32> {
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_migrations')",
            [],
            |row| row.get(0),
        )
        .map_err(|e| db_error("inspect schema version", e))?;
    if !exists {
        return Ok(0);
    }
    let max: Option<i64> = conn
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .map_err(|e| db_error("read schema version", e))?;
    Ok(max.unwrap_or(0).max(0) as u32)
}

fn backup_before_migration(conn: &Connection, path: &Path, from: u32) -> Result<PathBuf> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("ubuilder.db");
    let backup = path.with_file_name(format!(
        "{file_name}.v{from}-{}.bak",
        crate::telemetry::now_ms()
    ));
    let backup_text = backup.to_string_lossy().to_string();
    conn.execute("VACUUM INTO ?1", params![backup_text])
        .map_err(|e| db_error("back up database before migration", e))?;
    Ok(backup)
}

fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    declaration: &str,
) -> Result<()> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|e| db_error("inspect table columns", e))?;
    let exists = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| db_error("read table columns", e))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| db_error("read table columns", e))?
        .iter()
        .any(|name| name == column);
    if !exists {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {declaration}"),
            [],
        )
        .map_err(|e| db_error(format!("add {table}.{column}"), e))?;
    }
    Ok(())
}

fn new_lease_token(group: &str, owner: &str, now: u128) -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(1);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    crate::artifacts::sha256_bytes(
        format!(
            "{group}\0{owner}\0{now}\0{}\0{sequence}",
            std::process::id()
        )
        .as_bytes(),
    )
}

fn pipeline_run_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PipelineRunRecord> {
    Ok(PipelineRunRecord {
        id: row.get(0)?,
        workflow: row.get(1)?,
        source_path: PathBuf::from(row.get::<_, String>(2)?),
        source_digest: row.get(3)?,
        status: row.get(4)?,
        created_at: row.get::<_, i64>(5)? as u128,
        started_at: row.get::<_, Option<i64>>(6)?.map(|v| v as u128),
        finished_at: row.get::<_, Option<i64>>(7)?.map(|v| v as u128),
        host: row.get(8)?,
        inputs_json: row.get(9)?,
        error: row.get(10)?,
    })
}

fn configure(conn: &Connection) -> Result<()> {
    conn.busy_timeout(Duration::from_secs(15))
        .map_err(|e| db_error("busy timeout", e))?;
    // Switching journal mode needs a brief exclusive lock. SQLite can return
    // SQLITE_BUSY immediately for this pragma even with a busy timeout, so
    // concurrent `Cache::open`/`Db::open` calls retry until the lock clears.
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        match conn.pragma_update(None, "journal_mode", "WAL") {
            Ok(()) => break,
            Err(error) if is_busy(&error) && std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(db_error("wal mode", error)),
        }
    }
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|e| db_error("synchronous", e))?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|e| db_error("foreign keys", e))?;
    Ok(())
}

fn is_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy) | Some(rusqlite::ErrorCode::DatabaseLocked)
    )
}

fn bump_stat(conn: &Connection, name: &str, delta: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO cache_stats(name, value) VALUES (?1, ?2) ON CONFLICT(name) DO UPDATE SET value=value+?2",
        params![name, delta],
    )
    .map_err(|e| db_error("bump cache stat", e))?;
    Ok(())
}

fn read_stat(conn: &Connection, name: &str) -> Result<u64> {
    let value: i64 = conn
        .query_row(
            "SELECT value FROM cache_stats WHERE name=?1",
            params![name],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| db_error("read cache stat", e))?
        .unwrap_or(0);
    Ok(value.max(0) as u64)
}

fn db_error(context: impl std::fmt::Display, error: rusqlite::Error) -> UbuilderError {
    UbuilderError::new(ErrorCode::Internal, format!("sqlite {context}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_lifecycle_and_artifacts() {
        let db = Db::open_in_memory().unwrap();
        let job = JobRecord {
            id: "job1".into(),
            project: "demo".into(),
            framework: "rust".into(),
            target: "windows-x64".into(),
            host: "windows-x64".into(),
            status: "running".into(),
            started_at: 1,
            finished_at: None,
            manifest_path: None,
            log_path: None,
            error: None,
        };
        db.upsert_job(&job).unwrap();
        db.finish_job("job1", "success", None, None, 2).unwrap();
        assert_eq!(db.recent_jobs(1).unwrap()[0].status, "success");

        db.add_artifact(&ArtifactRecord {
            id: None,
            job_id: "job1".into(),
            target: "windows-x64".into(),
            framework: "rust".into(),
            kind: "executable".into(),
            format: "zip".into(),
            path: PathBuf::from("dist/app.zip"),
            sha256: "abc".into(),
            size: 10,
            validation: None,
            created_at: 1,
        })
        .unwrap();
        assert_eq!(db.list_artifacts(Some("windows-x64")).unwrap().len(), 1);
    }

    #[test]
    fn newer_schema_is_rejected_before_ddl() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("newer.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL);
                 INSERT INTO schema_migrations(version, applied_at) VALUES (99, 1);",
            )
            .unwrap();
        }
        let error = match Db::open(&path) {
            Ok(_) => panic!("newer schema must be rejected"),
            Err(error) => error,
        };
        assert_eq!(error.code, ErrorCode::SchemaUnsupported, "{error}");
        let conn = Connection::open(&path).unwrap();
        let jobs: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='jobs'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(jobs, 0, "rejection must happen before any DDL");
    }

    #[test]
    fn version_one_database_is_backed_up_and_migrated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ubuilder.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL);
                 INSERT INTO schema_migrations(version, applied_at) VALUES (1, 1);
                 CREATE TABLE pipeline_concurrency_slots (
                     group_name TEXT NOT NULL,
                     slot INTEGER NOT NULL,
                     owner TEXT NOT NULL,
                     lease_until INTEGER NOT NULL,
                     PRIMARY KEY (group_name, slot)
                 );",
            )
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
        let backups = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".bak"))
            .count();
        assert_eq!(backups, 1, "upgrade must leave a consistent backup");
        let conn = Connection::open(&path).unwrap();
        let mut stmt = conn
            .prepare("PRAGMA table_info(pipeline_concurrency_slots)")
            .unwrap();
        let columns = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(columns.iter().any(|column| column == "token"));
        assert!(columns.iter().any(|column| column == "generation"));
    }

    #[test]
    fn expired_lease_cannot_be_renewed_or_released_by_the_old_owner() {
        let db = Db::open_in_memory().unwrap();
        let first = db
            .acquire_concurrency_lease("build", "run-a/job", 1, 100, 10)
            .unwrap()
            .expect("first lease");
        assert!(db
            .acquire_concurrency_lease("build", "run-b/job", 1, 105, 10)
            .unwrap()
            .is_none());
        let second = db
            .acquire_concurrency_lease("build", "run-a/job", 1, 111, 10)
            .unwrap()
            .expect("expired slot is reclaimable");
        assert_ne!(first.token, second.token);
        assert!(second.generation > first.generation);
        assert!(!db.renew_concurrency_lease(&first, 120, 10).unwrap());
        assert!(!db.release_concurrency_lease(&first).unwrap());
        assert!(db.release_concurrency_lease(&second).unwrap());
        assert!(db.concurrency_leases("build").unwrap().is_empty());
    }

    #[test]
    fn terminal_run_state_cannot_transition_back_to_running() {
        let db = Db::open_in_memory().unwrap();
        db.insert_pipeline_run(&PipelineRunRecord {
            id: "run1".into(),
            workflow: "ci".into(),
            source_path: PathBuf::from("ci.yml"),
            source_digest: "digest".into(),
            status: "running".into(),
            created_at: 1,
            started_at: Some(1),
            finished_at: None,
            host: "windows-x64".into(),
            inputs_json: "{}".into(),
            error: None,
        })
        .unwrap();
        db.update_pipeline_run("run1", "success", None, Some(2))
            .unwrap();
        let error = db
            .update_pipeline_run("run1", "running", None, None)
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::StateInvalid, "{error}");
    }

    #[test]
    fn cache_hit_updates_stats() {
        let db = Db::open_in_memory().unwrap();
        db.cache_insert(&CacheRecord {
            key: "k".into(),
            kind: "tree".into(),
            path: PathBuf::from("cas/aa"),
            size: 5,
            created_at: 1,
            last_used: 1,
            hits: 0,
        })
        .unwrap();
        assert!(db.cache_record_hit("k", 2).unwrap().is_some());
        let stats = db.cache_stats().unwrap();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.entries, 1);
    }
}
