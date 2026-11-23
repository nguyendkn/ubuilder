use super::{duration_from_str, parse_duration, MatrixStrategy, StepDefinition};
use crate::error::{ErrorCode, Result, UbuilderError};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub type JobId = String;

/// Raw workflow document. `deny_unknown_fields` everywhere makes typos fail
/// with a file/line/column from the YAML deserializer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDefinition {
    pub version: u32,
    pub name: String,
    #[serde(rename = "on", default)]
    pub triggers: super::TriggerConfig,
    #[serde(default)]
    pub env: IndexMap<String, String>,
    #[serde(default)]
    pub defaults: WorkflowDefaults,
    #[serde(deserialize_with = "deserialize_jobs")]
    pub jobs: IndexMap<JobId, JobDefinition>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDefaults {
    #[serde(default)]
    pub shell: Option<String>,
    #[serde(default, rename = "working-directory")]
    pub working_directory: Option<String>,
}

fn deserialize_jobs<'de, D>(
    deserializer: D,
) -> std::result::Result<IndexMap<JobId, JobDefinition>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct JobsVisitor;
    impl<'de> serde::de::Visitor<'de> for JobsVisitor {
        type Value = IndexMap<JobId, JobDefinition>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a mapping of job ids to job definitions")
        }

        fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
        where
            A: serde::de::MapAccess<'de>,
        {
            let mut jobs = IndexMap::new();
            while let Some((id, job)) = map.next_entry::<JobId, JobDefinition>()? {
                if jobs.contains_key(&id) {
                    return Err(serde::de::Error::custom(format!("duplicate job id '{id}'")));
                }
                jobs.insert(id, job);
            }
            Ok(jobs)
        }
    }
    deserializer.deserialize_map(JobsVisitor)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobDefinition {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub needs: Vec<JobId>,
    #[serde(rename = "runs-on", default)]
    pub runner: RunnerSelector,
    #[serde(rename = "if", default)]
    pub condition: Option<String>,
    #[serde(default)]
    pub strategy: Option<MatrixStrategy>,
    #[serde(default)]
    pub env: IndexMap<String, String>,
    #[serde(default)]
    pub steps: Vec<StepDefinition>,
    #[serde(default, deserialize_with = "duration_from_str")]
    pub timeout: Option<Duration>,
    #[serde(default)]
    pub retry: Option<RetryPolicy>,
    #[serde(default)]
    pub concurrency: Option<ConcurrencyPolicy>,
    #[serde(default)]
    pub outputs: IndexMap<String, String>,
}

impl JobDefinition {
    pub fn display_name(&self, id: &str) -> String {
        self.name.clone().unwrap_or_else(|| id.to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RunnerSelector {
    Auto(String),
    Explicit(RunnerSpec),
}

impl Default for RunnerSelector {
    fn default() -> Self {
        RunnerSelector::Auto("auto".into())
    }
}

impl RunnerSelector {
    pub fn auto(&self) -> bool {
        matches!(self, RunnerSelector::Auto(value) if value == "auto")
    }

    pub fn spec(&self) -> RunnerSpec {
        match self {
            RunnerSelector::Auto(_) => RunnerSpec::default(),
            RunnerSelector::Explicit(spec) => spec.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerSpec {
    #[serde(default)]
    pub os: Option<String>,
    #[serde(default)]
    pub arch: Option<String>,
    #[serde(default)]
    pub backend: Option<ExecutionBackend>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutionBackend {
    Native,
    Container,
    Wsl,
}

impl ExecutionBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            ExecutionBackend::Native => "native",
            ExecutionBackend::Container => "container",
            ExecutionBackend::Wsl => "wsl",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetryPolicy {
    #[serde(rename = "max-attempts")]
    pub max_attempts: u32,
    #[serde(default, deserialize_with = "duration_from_str")]
    pub delay: Option<Duration>,
    #[serde(default)]
    pub backoff: Backoff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Backoff {
    #[default]
    Fixed,
    Exponential,
}

impl RetryPolicy {
    pub fn validate(&self) -> Result<()> {
        if self.max_attempts == 0 || self.max_attempts > 10 {
            return Err(UbuilderError::new(
                ErrorCode::ConfigInvalid,
                format!(
                    "retry.max-attempts must be between 1 and 10 (got {})",
                    self.max_attempts
                ),
            ));
        }
        Ok(())
    }

    pub fn delay_for(&self, attempt: u32) -> Duration {
        let base = self.delay.unwrap_or_else(|| Duration::from_secs(1));
        match self.backoff {
            Backoff::Fixed => base,
            Backoff::Exponential => {
                let factor = 1u32 << attempt.saturating_sub(1).min(10);
                base.saturating_mul(factor)
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConcurrencyPolicy {
    pub group: Option<String>,
    #[serde(rename = "cancel-in-progress", default)]
    pub cancel_in_progress: bool,
    #[serde(default)]
    pub max: Option<usize>,
}

impl ConcurrencyPolicy {
    pub fn validate(&self) -> Result<()> {
        if self.group.is_none() && self.max.is_none() {
            return Err(UbuilderError::new(
                ErrorCode::ConfigInvalid,
                "concurrency requires 'group' or 'max'",
            ));
        }
        if let Some(max) = self.max {
            if max == 0 || max > 64 {
                return Err(UbuilderError::new(
                    ErrorCode::ConfigInvalid,
                    format!("concurrency.max must be between 1 and 64 (got {max})"),
                ));
            }
        }
        Ok(())
    }
}

/// Used by the compiler to surface duration parse errors consistently.
pub fn job_timeout(job: &JobDefinition) -> Result<Option<Duration>> {
    job.timeout
        .map(|duration| {
            if duration.is_zero() {
                Err(UbuilderError::new(
                    ErrorCode::ConfigInvalid,
                    "job timeout must be greater than zero",
                ))
            } else {
                Ok(duration)
            }
        })
        .transpose()
}

pub fn parse_timeout(raw: &str) -> Result<Duration> {
    parse_duration(raw)
}
