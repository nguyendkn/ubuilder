//! Diagnostics shared by validation, compilation and the CLI.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: String,
    pub message: String,
    pub path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

impl Diagnostic {
    pub fn error(path: &Path, code: &str, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            code: code.into(),
            message: message.into(),
            path: path.to_path_buf(),
            line: None,
            column: None,
            job: None,
            step: None,
            remediation: None,
        }
    }

    pub fn warning(path: &Path, code: &str, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            ..Self::error(path, code, message)
        }
    }

    pub fn at(mut self, line: Option<usize>, column: Option<usize>) -> Self {
        self.line = line;
        self.column = column;
        self
    }

    pub fn job(mut self, job: impl Into<String>) -> Self {
        self.job = Some(job.into());
        self
    }

    pub fn step(mut self, step: impl Into<String>) -> Self {
        self.step = Some(step.into());
        self
    }

    pub fn remediation(mut self, text: impl Into<String>) -> Self {
        self.remediation = Some(text.into());
        self
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }

    pub fn location(&self) -> String {
        match (self.line, self.column) {
            (Some(line), Some(column)) => format!("{}:{line}:{column}", self.path.display()),
            (Some(line), None) => format!("{}:{line}", self.path.display()),
            _ => self.path.display().to_string(),
        }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} [{}] {}: {}",
            self.location(),
            self.code,
            match self.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
            },
            self.message
        )?;
        if let Some(job) = &self.job {
            write!(f, " (job '{job}')")?;
        }
        if let Some(step) = &self.step {
            write!(f, " (step '{step}')")?;
        }
        if let Some(remediation) = &self.remediation {
            write!(f, "; {remediation}")?;
        }
        Ok(())
    }
}

pub fn render(diagnostics: &[Diagnostic]) -> String {
    diagnostics
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn has_errors(diagnostics: &[Diagnostic]) -> bool {
    diagnostics.iter().any(Diagnostic::is_error)
}
