//! Typed lifecycle states shared by runs, jobs and steps.
//!
//! Persisted strings keep their historical values (`success`, `failure`,
//! `queued`) so existing databases and reports remain readable. Transitions are
//! validated before a row is updated.

use crate::error::{ErrorCode, Result, UbuilderError};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunState {
    Created,
    Pending,
    Ready,
    Running,
    Succeeded,
    Failed,
    Skipped,
    Blocked,
    Cancelled,
    TimedOut,
    Interrupted,
}

impl RunState {
    pub const ALL: [RunState; 11] = [
        RunState::Created,
        RunState::Pending,
        RunState::Ready,
        RunState::Running,
        RunState::Succeeded,
        RunState::Failed,
        RunState::Skipped,
        RunState::Blocked,
        RunState::Cancelled,
        RunState::TimedOut,
        RunState::Interrupted,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            RunState::Created => "created",
            RunState::Pending => "queued",
            RunState::Ready => "ready",
            RunState::Running => "running",
            RunState::Succeeded => "success",
            RunState::Failed => "failure",
            RunState::Skipped => "skipped",
            RunState::Blocked => "blocked",
            RunState::Cancelled => "cancelled",
            RunState::TimedOut => "timed_out",
            RunState::Interrupted => "interrupted",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "created" => RunState::Created,
            "pending" | "queued" => RunState::Pending,
            "ready" => RunState::Ready,
            "running" => RunState::Running,
            "success" | "succeeded" => RunState::Succeeded,
            "failure" | "failed" => RunState::Failed,
            "skipped" => RunState::Skipped,
            "blocked" => RunState::Blocked,
            "cancelled" | "canceled" => RunState::Cancelled,
            "timed_out" | "timeout" => RunState::TimedOut,
            "interrupted" => RunState::Interrupted,
            _ => return None,
        })
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            RunState::Succeeded
                | RunState::Failed
                | RunState::Skipped
                | RunState::Blocked
                | RunState::Cancelled
                | RunState::TimedOut
                | RunState::Interrupted
        )
    }

    /// Whether `to` is a legal next state for this entity.
    pub fn can_transition(self, to: RunState, entity: StateEntity) -> bool {
        if self == to {
            return true;
        }
        use RunState::*;
        let allowed: &[RunState] = match (entity, self) {
            (StateEntity::Run, Created) => &[Pending, Ready, Running, Cancelled],
            (StateEntity::Run, Pending) => &[Ready, Running, Cancelled, Blocked],
            (StateEntity::Run, Ready) => &[Running, Cancelled, Blocked],
            (StateEntity::Run, Running) => &[Succeeded, Failed, Cancelled, TimedOut, Interrupted],
            (StateEntity::Run, _) => &[],
            (StateEntity::Job, Created) => &[Pending, Ready, Running, Skipped, Blocked, Cancelled],
            (StateEntity::Job, Pending) => &[Ready, Running, Skipped, Blocked, Cancelled],
            (StateEntity::Job, Ready) => &[Running, Skipped, Blocked, Cancelled],
            (StateEntity::Job, Running) => {
                &[Succeeded, Failed, Cancelled, TimedOut, Interrupted, Blocked]
            }
            (StateEntity::Job, _) => &[],
            (StateEntity::Step, Created) => &[Pending, Ready, Running, Skipped, Cancelled],
            (StateEntity::Step, Pending) => &[Ready, Running, Skipped, Cancelled],
            (StateEntity::Step, Ready) => &[Running, Skipped, Cancelled],
            (StateEntity::Step, Running) => &[Succeeded, Failed, Cancelled, TimedOut, Interrupted],
            (StateEntity::Step, _) => &[],
        };
        allowed.contains(&to)
    }
}

impl fmt::Display for RunState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl PartialEq<str> for RunState {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for RunState {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl Serialize for RunState {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RunState {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        RunState::parse(&raw)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown run state '{raw}'")))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateEntity {
    Run,
    Job,
    Step,
}

impl StateEntity {
    pub fn as_str(self) -> &'static str {
        match self {
            StateEntity::Run => "run",
            StateEntity::Job => "job",
            StateEntity::Step => "step",
        }
    }
}

pub fn validate_transition(entity: StateEntity, from: &str, to: RunState) -> Result<()> {
    let from = RunState::parse(from).ok_or_else(|| {
        UbuilderError::new(
            ErrorCode::StateInvalid,
            format!("corrupt persisted {} state '{from}'", entity.as_str()),
        )
    })?;
    if from.can_transition(to, entity) {
        Ok(())
    } else {
        Err(UbuilderError::new(
            ErrorCode::StateInvalid,
            format!("invalid {} transition {} -> {}", entity.as_str(), from, to),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_states_do_not_restart() {
        assert!(RunState::Succeeded.is_terminal());
        assert!(!RunState::Succeeded.can_transition(RunState::Running, StateEntity::Run));
        assert!(RunState::Running.can_transition(RunState::Failed, StateEntity::Job));
        assert!(!RunState::Pending.can_transition(RunState::Succeeded, StateEntity::Job));
    }

    #[test]
    fn legacy_strings_roundtrip() {
        assert_eq!(RunState::parse("success"), Some(RunState::Succeeded));
        assert_eq!(RunState::Succeeded.as_str(), "success");
        assert_eq!(RunState::Pending.as_str(), "queued");
    }
}
