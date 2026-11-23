use super::RetryPolicy;
use crate::error::{ErrorCode, Result, UbuilderError};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Strongly typed step: command steps and built-in action steps are distinct.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StepDefinition {
    Run(CommandStep),
    Action(ActionStep),
}

impl StepDefinition {
    pub fn name(&self, index: usize) -> String {
        match self {
            StepDefinition::Run(step) => step
                .name
                .clone()
                .unwrap_or_else(|| format!("Run (step {})", index + 1)),
            StepDefinition::Action(step) => step.name.clone().unwrap_or_else(|| step.uses.clone()),
        }
    }

    pub fn condition(&self) -> Option<&str> {
        match self {
            StepDefinition::Run(step) => step.condition.as_deref(),
            StepDefinition::Action(step) => step.condition.as_deref(),
        }
    }

    pub fn continue_on_error(&self) -> bool {
        match self {
            StepDefinition::Run(step) => step.continue_on_error,
            StepDefinition::Action(step) => step.continue_on_error,
        }
    }

    pub fn timeout(&self) -> Option<Duration> {
        match self {
            StepDefinition::Run(step) => step.timeout,
            StepDefinition::Action(step) => step.timeout,
        }
    }

    pub fn retry(&self) -> Option<&RetryPolicy> {
        match self {
            StepDefinition::Run(step) => step.retry.as_ref(),
            StepDefinition::Action(step) => step.retry.as_ref(),
        }
    }

    pub fn env(&self) -> &IndexMap<String, String> {
        match self {
            StepDefinition::Run(step) => &step.env,
            StepDefinition::Action(step) => &step.env,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            StepDefinition::Run(_) => "run",
            StepDefinition::Action(_) => "uses",
        }
    }

    pub fn id(&self) -> Option<&str> {
        match self {
            StepDefinition::Run(step) => step.id.as_deref(),
            StepDefinition::Action(step) => step.id.as_deref(),
        }
    }

    pub fn validate(&self) -> Result<()> {
        match self {
            StepDefinition::Run(step) => step.validate(),
            StepDefinition::Action(step) => step.validate(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandStep {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    pub run: String,
    #[serde(default)]
    pub shell: Option<String>,
    #[serde(default, rename = "working-directory")]
    pub working_directory: Option<String>,
    #[serde(default, rename = "if")]
    pub condition: Option<String>,
    #[serde(default, rename = "continue-on-error")]
    pub continue_on_error: bool,
    #[serde(default, deserialize_with = "super::duration_from_str")]
    pub timeout: Option<Duration>,
    #[serde(default)]
    pub retry: Option<RetryPolicy>,
    #[serde(default)]
    pub env: IndexMap<String, String>,
}

impl CommandStep {
    fn validate(&self) -> Result<()> {
        if self.run.trim().is_empty() {
            return Err(UbuilderError::new(
                ErrorCode::ConfigInvalid,
                "run step has an empty command",
            ));
        }
        if let Some(shell) = &self.shell {
            if !matches!(
                shell.as_str(),
                "sh" | "bash" | "pwsh" | "powershell" | "cmd" | "python"
            ) {
                return Err(UbuilderError::new(
                    ErrorCode::ConfigInvalid,
                    format!(
                        "unsupported shell '{shell}' (sh, bash, pwsh, powershell, cmd, python)"
                    ),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionStep {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    pub uses: String,
    #[serde(default)]
    pub with: IndexMap<String, serde_yaml_ng::Value>,
    #[serde(default, rename = "if")]
    pub condition: Option<String>,
    #[serde(default, rename = "continue-on-error")]
    pub continue_on_error: bool,
    #[serde(default, deserialize_with = "super::duration_from_str")]
    pub timeout: Option<Duration>,
    #[serde(default)]
    pub retry: Option<RetryPolicy>,
    #[serde(default)]
    pub env: IndexMap<String, String>,
}

impl ActionStep {
    fn validate(&self) -> Result<()> {
        if self.uses.trim().is_empty() {
            return Err(UbuilderError::new(
                ErrorCode::ConfigInvalid,
                "uses step has an empty action identifier",
            ));
        }
        Ok(())
    }
}

pub fn scalar_to_string(value: &serde_yaml_ng::Value) -> Result<String> {
    match value {
        serde_yaml_ng::Value::String(text) => Ok(text.clone()),
        serde_yaml_ng::Value::Bool(flag) => Ok(flag.to_string()),
        serde_yaml_ng::Value::Number(number) => Ok(number.to_string()),
        serde_yaml_ng::Value::Null => Ok(String::new()),
        _ => Err(UbuilderError::new(
            ErrorCode::ConfigInvalid,
            "action input values must be scalars (string, number, boolean or null)",
        )),
    }
}
