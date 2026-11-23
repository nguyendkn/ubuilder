//! Read-only run inspection and stale-run recovery.

use crate::error::{ErrorCode, Result, UbuilderError};
use crate::storage::{
    Db, PipelineArtifactRecord, PipelineJobRecord, PipelineRunRecord, PipelineStepRecord,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunStatus {
    pub run: PipelineRunRecord,
    pub jobs: Vec<PipelineJobRecord>,
    pub steps: Vec<PipelineStepRecord>,
    pub artifacts: Vec<PipelineArtifactRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub job_id: String,
    pub step_id: String,
    pub path: PathBuf,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoverReport {
    pub recovered: Vec<String>,
    pub stale_before: u128,
}

pub fn status(db: &Db, run_id: &str) -> Result<RunStatus> {
    let run = db.pipeline_run(run_id)?.ok_or_else(|| {
        UbuilderError::new(ErrorCode::Usage, format!("unknown pipeline run '{run_id}'"))
            .remediation("Run `ubuilder pipeline history` to list recorded runs")
    })?;
    Ok(RunStatus {
        jobs: db.pipeline_jobs(run_id)?,
        steps: db.pipeline_steps(run_id)?,
        artifacts: db.pipeline_artifacts(run_id)?,
        run,
    })
}

pub fn history(db: &Db, limit: usize) -> Result<Vec<PipelineRunRecord>> {
    db.pipeline_runs(limit)
}

pub fn logs(db: &Db, run_id: &str, job: Option<&str>, step: Option<&str>) -> Result<Vec<LogEntry>> {
    let mut entries = Vec::new();
    for record in db.pipeline_steps(run_id)? {
        if job.is_some_and(|value| value != record.job_id) {
            continue;
        }
        if step.is_some_and(|value| value != record.step_id) {
            continue;
        }
        let Some(path) = record.log_path else {
            continue;
        };
        if !path.is_file() {
            continue;
        }
        let bytes = std::fs::metadata(&path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        entries.push(LogEntry {
            job_id: record.job_id,
            step_id: record.step_id,
            path,
            bytes,
        });
    }
    Ok(entries)
}

pub fn recover(db: &Db, stale_before: u128) -> Result<RecoverReport> {
    Ok(RecoverReport {
        recovered: db.recover_stale_pipeline_runs(stale_before)?,
        stale_before,
    })
}

/// Marker checked by the runtime to cancel a run started by another process.
pub fn cancel_marker(data_dir: &std::path::Path, run_id: &str) -> PathBuf {
    data_dir
        .join("pipeline")
        .join("runs")
        .join(run_id)
        .join("cancel")
}
