use super::ast::Value;
use std::collections::BTreeSet;

pub trait ValueExt {
    fn truthy(&self) -> bool;
    fn as_template_string(&self) -> String;
}

impl ValueExt for Value {
    fn truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(flag) => *flag,
            Value::Number(number) => number.as_f64().map(|n| n != 0.0).unwrap_or(false),
            Value::String(text) => !text.is_empty(),
            Value::Array(_) | Value::Object(_) => true,
        }
    }

    fn as_template_string(&self) -> String {
        match self {
            Value::Null => String::new(),
            Value::String(text) => text.clone(),
            Value::Bool(flag) => flag.to_string(),
            Value::Number(number) => number.to_string(),
            other => other.to_string(),
        }
    }
}

/// Runtime context. Absent roots are null unless they are explicitly marked
/// unavailable for the current phase, which produces an actionable error.
#[derive(Debug, Clone, Default)]
pub struct Context {
    values: serde_json::Map<String, Value>,
    unavailable: BTreeSet<String>,
    pub success: bool,
    pub failure: bool,
    pub cancelled: bool,
    pub steps_outputs: serde_json::Map<String, Value>,
}

impl Context {
    pub fn new() -> Self {
        Self {
            success: true,
            ..Self::default()
        }
    }

    pub fn with(mut self, key: impl Into<String>, value: Value) -> Self {
        self.values.insert(key.into(), value);
        self
    }

    pub fn insert(&mut self, key: impl Into<String>, value: Value) {
        self.values.insert(key.into(), value);
    }

    pub fn mark_unavailable(&mut self, key: impl Into<String>) {
        self.unavailable.insert(key.into());
    }

    pub fn get_root(&self, key: &str) -> Option<&Value> {
        self.values.get(key)
    }

    pub fn is_unavailable(&self, key: &str) -> bool {
        self.unavailable.contains(key)
    }

    pub fn steps(&self) -> &serde_json::Map<String, Value> {
        &self.steps_outputs
    }

    pub fn to_value(&self) -> Value {
        Value::Object(self.values.clone())
    }
}

/// A glob-like name pattern supporting `*`; used by artifact download.
pub fn pattern_matches(pattern: &str, value: &str) -> bool {
    if pattern == "*" || pattern == value {
        return true;
    }
    match pattern.split_once('*') {
        Some((prefix, suffix)) => value.starts_with(prefix) && value.ends_with(suffix),
        None => false,
    }
}
