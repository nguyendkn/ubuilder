//! Structured pipeline events. JSON mode writes one object per line to stderr;
//! stdout remains reserved for the final result envelope.

use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    RunStart,
    RunFinish,
    JobStart,
    JobFinish,
    StepStart,
    StepFinish,
    StepSkipped,
    Retry,
    CacheHit,
    CacheMiss,
    Artifact,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct PipelineEvent {
    pub event: EventKind,
    pub timestamp: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "serde_json::Value::is_null")]
    pub data: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct Events {
    json: bool,
    quiet: bool,
    verbose: bool,
}

impl Events {
    pub fn new(json: bool, quiet: bool, verbose: bool) -> Self {
        Self {
            json,
            quiet,
            verbose,
        }
    }

    pub fn emit(&self, event: PipelineEvent) {
        if self.quiet && !matches!(event.event, EventKind::Error | EventKind::RunFinish) {
            return;
        }
        if self.json {
            if let Ok(line) = serde_json::to_string(&event) {
                eprintln!("{line}");
            }
        } else {
            let mut prefix = String::new();
            if let Some(run) = &event.run_id {
                prefix.push_str(&format!("[{run}] "));
            }
            if let Some(job) = &event.job_id {
                prefix.push_str(&format!("{job} "));
            }
            if let Some(step) = &event.step_id {
                prefix.push_str(&format!("› {step} "));
            }
            eprintln!("{prefix}{}", event.message);
            if self.verbose && !event.data.is_null() {
                eprintln!("  {}", redact_value(&event.data));
            }
        }
    }

    pub fn run_start(&self, run_id: &str, workflow: &str) {
        self.emit(PipelineEvent {
            event: EventKind::RunStart,
            timestamp: crate::telemetry::now_ms(),
            run_id: Some(run_id.into()),
            job_id: None,
            step_id: None,
            message: format!("run started: {workflow}"),
            data: json!({ "workflow": workflow }),
        });
    }

    pub fn run_finish(&self, run_id: &str, status: &str, duration_ms: u128) {
        self.emit(PipelineEvent {
            event: EventKind::RunFinish,
            timestamp: crate::telemetry::now_ms(),
            run_id: Some(run_id.into()),
            job_id: None,
            step_id: None,
            message: format!("run finished: {status} ({duration_ms} ms)"),
            data: json!({ "status": status, "duration_ms": duration_ms }),
        });
    }

    pub fn job(&self, kind: EventKind, run_id: &str, job_id: &str, message: String) {
        self.emit(PipelineEvent {
            event: kind,
            timestamp: crate::telemetry::now_ms(),
            run_id: Some(run_id.into()),
            job_id: Some(job_id.into()),
            step_id: None,
            message,
            data: serde_json::Value::Null,
        });
    }

    pub fn step(
        &self,
        kind: EventKind,
        run_id: &str,
        job_id: &str,
        step_id: &str,
        message: String,
    ) {
        self.emit(PipelineEvent {
            event: kind,
            timestamp: crate::telemetry::now_ms(),
            run_id: Some(run_id.into()),
            job_id: Some(job_id.into()),
            step_id: Some(step_id.into()),
            message,
            data: serde_json::Value::Null,
        });
    }

    pub fn warning(&self, run_id: &str, message: String) {
        self.emit(PipelineEvent {
            event: EventKind::Warning,
            timestamp: crate::telemetry::now_ms(),
            run_id: Some(run_id.into()),
            job_id: None,
            step_id: None,
            message,
            data: serde_json::Value::Null,
        });
    }

    pub fn error(&self, run_id: &str, message: String) {
        self.emit(PipelineEvent {
            event: EventKind::Error,
            timestamp: crate::telemetry::now_ms(),
            run_id: Some(run_id.into()),
            job_id: None,
            step_id: None,
            message,
            data: serde_json::Value::Null,
        });
    }
}

/// Redact obviously secret values before writing verbose event data.
pub fn redact_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut redacted = serde_json::Map::new();
            for (key, value) in map {
                let lower = key.to_ascii_lowercase();
                if lower.contains("token")
                    || lower.contains("password")
                    || lower.contains("secret")
                    || lower.contains("key")
                {
                    redacted.insert(key.clone(), json!("***"));
                } else {
                    redacted.insert(key.clone(), redact_value(value));
                }
            }
            serde_json::Value::Object(redacted)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(redact_value).collect())
        }
        other => other.clone(),
    }
}
