//! Error model with stable machine-readable error codes.

use serde::{Deserialize, Serialize};
use std::fmt;

pub type Result<T, E = UbuilderError> = std::result::Result<T, E>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    Usage,
    ProjectNotFound,
    AmbiguousProject,
    ConfigInvalid,
    UnsupportedTarget,
    ToolchainMissing,
    ExecutionFailed,
    Timeout,
    Cancelled,
    CacheCorrupt,
    ArtifactInvalid,
    SigningUnsupported,
    ApprovalRequired,
    NotVerified,
    Io,
    Json,
    Internal,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::Usage => "USAGE",
            ErrorCode::ProjectNotFound => "PROJECT_NOT_FOUND",
            ErrorCode::AmbiguousProject => "AMBIGUOUS_PROJECT",
            ErrorCode::ConfigInvalid => "CONFIG_INVALID",
            ErrorCode::UnsupportedTarget => "UNSUPPORTED_TARGET",
            ErrorCode::ToolchainMissing => "TOOLCHAIN_MISSING",
            ErrorCode::ExecutionFailed => "EXECUTION_FAILED",
            ErrorCode::Timeout => "TIMEOUT",
            ErrorCode::Cancelled => "CANCELLED",
            ErrorCode::CacheCorrupt => "CACHE_CORRUPT",
            ErrorCode::ArtifactInvalid => "ARTIFACT_INVALID",
            ErrorCode::SigningUnsupported => "SIGNING_UNSUPPORTED",
            ErrorCode::ApprovalRequired => "APPROVAL_REQUIRED",
            ErrorCode::NotVerified => "NOT_VERIFIED",
            ErrorCode::Io => "IO",
            ErrorCode::Json => "JSON",
            ErrorCode::Internal => "INTERNAL",
        }
    }

    pub fn exit_code(self) -> i32 {
        match self {
            ErrorCode::Usage => 2,
            ErrorCode::ProjectNotFound
            | ErrorCode::AmbiguousProject
            | ErrorCode::ConfigInvalid
            | ErrorCode::UnsupportedTarget
            | ErrorCode::ToolchainMissing
            | ErrorCode::ApprovalRequired => 3,
            ErrorCode::Cancelled => 130,
            _ => 1,
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{code}: {message}")]
pub struct UbuilderError {
    pub code: ErrorCode,
    pub message: String,
    pub remediation: Option<String>,
    pub details: Option<serde_json::Value>,
    #[source]
    pub source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl UbuilderError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            remediation: None,
            details: None,
            source: None,
        }
    }

    pub fn remediation(mut self, text: impl Into<String>) -> Self {
        self.remediation = Some(text.into());
        self
    }

    pub fn details(mut self, value: serde_json::Value) -> Self {
        self.details = Some(value);
        self
    }

    pub fn source<E>(mut self, error: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        self.source = Some(Box::new(error));
        self
    }

    pub fn io(context: impl fmt::Display, error: std::io::Error) -> Self {
        Self::new(ErrorCode::Io, format!("{context}: {error}")).source(error)
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "ok": false,
            "error": {
                "code": self.code,
                "message": self.message,
                "remediation": self.remediation,
                "details": self.details,
            }
        })
    }
}

pub fn usage(message: impl Into<String>) -> UbuilderError {
    UbuilderError::new(ErrorCode::Usage, message)
}
