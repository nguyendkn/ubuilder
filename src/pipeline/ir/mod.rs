//! Serializable intermediate representation consumed by the planner/runtime.

use crate::pipeline::expression::{Expression, Template};
use crate::pipeline::schema::{
    Backoff, ConcurrencyPolicy, ExecutionBackend, RetryPolicy, RunnerSpec,
};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowIr {
    pub version: u32,
    pub name: String,
    pub source_path: PathBuf,
    pub source_digest: String,
    pub env: IndexMap<String, Template>,
    pub jobs: Vec<IrJob>,
    /// Jobs in deterministic dependency order.
    pub order: Vec<String>,
    pub waves: Vec<Vec<String>>,
    pub max_matrix_jobs: usize,
}

impl WorkflowIr {
    pub fn job(&self, id: &str) -> Option<&IrJob> {
        self.jobs.iter().find(|job| job.id == id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IrJob {
    pub id: String,
    pub base_id: String,
    pub name: String,
    pub needs: Vec<String>,
    pub base_needs: Vec<String>,
    pub runner: IrRunner,
    pub condition: Option<Expression>,
    pub env: IndexMap<String, Template>,
    pub steps: Vec<IrStep>,
    pub timeout_secs: Option<u64>,
    pub retry: Option<RetryPolicy>,
    pub concurrency: Option<ConcurrencyPolicy>,
    pub outputs: IndexMap<String, Template>,
    pub matrix: IndexMap<String, serde_json::Value>,
    pub fail_fast: bool,
    pub max_parallel: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IrRunner {
    pub requested: RunnerSpec,
    pub os: String,
    pub arch: String,
    pub backend: ExecutionBackend,
    pub native: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IrStep {
    pub id: String,
    pub index: usize,
    pub name: String,
    pub condition: Option<Expression>,
    pub continue_on_error: bool,
    pub timeout_secs: Option<u64>,
    pub retry: Option<RetryPolicy>,
    pub env: IndexMap<String, Template>,
    pub kind: IrStepKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum IrStepKind {
    Run {
        command: Template,
        shell: String,
        working_directory: Option<Template>,
    },
    Action {
        uses: String,
        with: IndexMap<String, Template>,
    },
}

impl Backoff {
    pub fn label(self) -> &'static str {
        match self {
            Backoff::Fixed => "fixed",
            Backoff::Exponential => "exponential",
        }
    }
}
