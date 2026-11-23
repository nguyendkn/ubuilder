//! Human and machine-readable progress output, plus structured JSONL logs.

use crate::error::{Result, UbuilderError};
use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    BuildStart,
    StepStart,
    StepFinish,
    TargetStart,
    TargetFinish,
    Artifact,
    Validation,
    Warning,
    Error,
    Summary,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProgressEvent {
    pub timestamp_ms: u128,
    pub event: EventKind,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

#[derive(Clone)]
pub struct Ui {
    json: bool,
    verbose: bool,
    quiet: bool,
    color: bool,
    log: Option<Arc<Mutex<File>>>,
}

impl Ui {
    pub fn new(json: bool, verbose: bool, quiet: bool) -> Self {
        let no_color = std::env::var_os("NO_COLOR").is_some();
        let color = !no_color
            && std::io::stdout().is_terminal()
            && std::env::var("TERM").map(|t| t != "dumb").unwrap_or(true);
        Self {
            json,
            verbose,
            quiet,
            color,
            log: None,
        }
    }

    pub fn with_log_file(mut self, path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| UbuilderError::io(format!("cannot create {}", parent.display()), e))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| UbuilderError::io(format!("cannot open log {}", path.display()), e))?;
        self.log = Some(Arc::new(Mutex::new(file)));
        Ok(self)
    }

    pub fn json(&self) -> bool {
        self.json
    }

    pub fn verbose(&self) -> bool {
        self.verbose
    }

    pub fn quiet(&self) -> bool {
        self.quiet
    }

    pub fn info(&self, message: impl AsRef<str>) {
        if !self.quiet && !self.json {
            println!("{}", message.as_ref());
        }
    }

    pub fn success(&self, message: impl AsRef<str>) {
        if !self.quiet && !self.json {
            println!("{}", self.paint("\x1b[32m", message.as_ref()));
        }
    }

    pub fn warn(&self, message: impl AsRef<str>) {
        if !self.quiet {
            if self.json {
                self.emit(EventKind::Warning, message.as_ref(), None, None, None);
            } else {
                eprintln!("{}", self.paint("\x1b[33m", message.as_ref()));
            }
        }
    }

    pub fn error(&self, message: impl AsRef<str>) {
        if self.json {
            self.emit(EventKind::Error, message.as_ref(), None, None, None);
        } else {
            eprintln!("{}", self.paint("\x1b[31m", message.as_ref()));
        }
    }

    pub fn event(
        &self,
        kind: EventKind,
        message: impl Into<String>,
        target: Option<&str>,
        step: Option<&str>,
        data: Option<serde_json::Value>,
    ) {
        self.emit(kind, &message.into(), target, step, data);
    }

    fn emit(
        &self,
        kind: EventKind,
        message: &str,
        target: Option<&str>,
        step: Option<&str>,
        data: Option<serde_json::Value>,
    ) {
        let event = ProgressEvent {
            timestamp_ms: now_ms(),
            event: kind,
            message: message.to_string(),
            target: target.map(str::to_string),
            step: step.map(str::to_string),
            data,
        };
        if let Some(log) = &self.log {
            if let Ok(line) = serde_json::to_string(&event) {
                if let Ok(mut file) = log.lock() {
                    let _ = writeln!(file, "{line}");
                    let _ = file.flush();
                }
            }
        }
        if self.json {
            if let Ok(line) = serde_json::to_string(&event) {
                eprintln!("{line}");
            }
        } else if !self.quiet {
            match kind {
                EventKind::Error => eprintln!("{}", self.paint("\x1b[31m", message)),
                EventKind::Warning => eprintln!("{}", self.paint("\x1b[33m", message)),
                EventKind::Artifact | EventKind::Validation => {
                    println!("{}", self.paint("\x1b[36m", message))
                }
                _ if self.verbose || matches!(kind, EventKind::Summary) => println!("{message}"),
                _ => {}
            }
        }
    }

    fn paint(&self, code: &str, text: &str) -> String {
        if self.color {
            format!("{code}{text}{}", self.reset())
        } else {
            text.to_string()
        }
    }

    fn reset(&self) -> &'static str {
        if self.color {
            "\x1b[0m"
        } else {
            ""
        }
    }
}

pub fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

pub fn status_badge(level: crate::capability::SupportLevel) -> &'static str {
    match level {
        crate::capability::SupportLevel::Supported => "READY",
        crate::capability::SupportLevel::Conditional => "CONDITIONAL",
        crate::capability::SupportLevel::Experimental => "EXPERIMENTAL",
        crate::capability::SupportLevel::Unsupported => "UNSUPPORTED",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_events_are_serializable_and_timestamped() {
        let event = ProgressEvent {
            timestamp_ms: now_ms(),
            event: EventKind::StepFinish,
            message: "ok".into(),
            target: Some("windows-x64".into()),
            step: Some("cargo-build".into()),
            data: None,
        };
        let text = serde_json::to_string(&event).unwrap();
        assert!(text.contains("\"event\":\"step_finish\""));
        assert!(text.contains("\"target\":\"windows-x64\""));
    }
}
