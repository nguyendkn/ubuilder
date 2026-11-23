//! Deterministic matrix expansion with include/exclude rules and hard limits.

use crate::error::{ErrorCode, Result, UbuilderError};
use crate::pipeline::schema::MatrixDefinition;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

pub const DEFAULT_MAX_MATRIX_JOBS: usize = 256;
pub const MAX_MATRIX_JOBS_LIMIT: usize = 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatrixCombination {
    pub values: IndexMap<String, serde_json::Value>,
}

impl MatrixCombination {
    pub fn suffix(&self) -> String {
        if self.values.is_empty() {
            return String::new();
        }
        let parts = self
            .values
            .iter()
            .map(|(key, value)| format!("{key}={}", scalar(value)))
            .collect::<Vec<_>>()
            .join(",");
        format!("[{parts}]")
    }
}

/// Expand a matrix. Iteration order is axis declaration order; include and
/// exclude entries are applied in document order.
pub fn expand(definition: &MatrixDefinition, max_jobs: usize) -> Result<Vec<MatrixCombination>> {
    let limit = max_jobs.clamp(1, MAX_MATRIX_JOBS_LIMIT);
    let mut combinations: Vec<IndexMap<String, serde_json::Value>> = vec![IndexMap::new()];
    for (axis, values) in &definition.axes {
        if values.is_empty() {
            return Err(UbuilderError::new(
                ErrorCode::ConfigInvalid,
                format!("matrix axis '{axis}' has no values"),
            ));
        }
        let mut next = Vec::new();
        for existing in &combinations {
            for value in values {
                validate_scalar(axis, value)?;
                let mut combination = existing.clone();
                combination.insert(axis.clone(), yaml_to_json(value));
                next.push(combination);
                if next.len() > limit {
                    return Err(UbuilderError::new(
                        ErrorCode::ConfigInvalid,
                        format!("matrix expands beyond the limit of {limit} jobs"),
                    ));
                }
            }
        }
        combinations = next;
    }

    for include in &definition.include {
        for value in include.values() {
            validate_scalar("include", value)?;
        }
        let include: IndexMap<String, serde_json::Value> = include
            .iter()
            .map(|(key, value)| (key.clone(), yaml_to_json(value)))
            .collect();
        let mut applied = false;
        for combination in combinations.iter_mut() {
            if compatible(combination, &include) {
                *combination = merge(combination, &include);
                applied = true;
            }
        }
        if !applied {
            combinations.push(include);
        }
        if combinations.len() > limit {
            return Err(UbuilderError::new(
                ErrorCode::ConfigInvalid,
                format!("matrix expands beyond the limit of {limit} jobs"),
            ));
        }
    }

    for exclude in &definition.exclude {
        let exclude: IndexMap<String, serde_json::Value> = exclude
            .iter()
            .map(|(key, value)| (key.clone(), yaml_to_json(value)))
            .collect();
        combinations.retain(|combo| !exact_match(combo, &exclude));
    }

    let mut seen = std::collections::BTreeSet::new();
    let mut result = Vec::new();
    for values in combinations {
        let combination = MatrixCombination { values };
        if !seen.insert(combination.suffix()) {
            return Err(UbuilderError::new(
                ErrorCode::ConfigInvalid,
                format!("duplicate matrix combination {}", combination.suffix()),
            ));
        }
        result.push(combination);
    }
    Ok(result)
}

fn compatible(
    combination: &IndexMap<String, serde_json::Value>,
    include: &IndexMap<String, serde_json::Value>,
) -> bool {
    include.iter().all(|(key, value)| {
        combination
            .get(key)
            .map(|existing| existing == value)
            .unwrap_or(true)
    })
}

fn exact_match(
    combination: &IndexMap<String, serde_json::Value>,
    exclude: &IndexMap<String, serde_json::Value>,
) -> bool {
    !exclude.is_empty()
        && exclude
            .iter()
            .all(|(key, value)| combination.get(key) == Some(value))
}

fn merge(
    combination: &IndexMap<String, serde_json::Value>,
    include: &IndexMap<String, serde_json::Value>,
) -> IndexMap<String, serde_json::Value> {
    let mut merged = combination.clone();
    for (key, value) in include {
        merged.insert(key.clone(), value.clone());
    }
    merged
}

fn scalar(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn validate_scalar(axis: &str, value: &serde_yaml_ng::Value) -> Result<()> {
    match value {
        serde_yaml_ng::Value::String(_)
        | serde_yaml_ng::Value::Number(_)
        | serde_yaml_ng::Value::Bool(_)
        | serde_yaml_ng::Value::Null => Ok(()),
        _ => Err(UbuilderError::new(
            ErrorCode::ConfigInvalid,
            format!("matrix axis '{axis}' values must be scalars"),
        )),
    }
}

fn yaml_to_json(value: &serde_yaml_ng::Value) -> serde_json::Value {
    serde_yaml_ng::from_value(value.clone()).unwrap_or(serde_json::Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition(source: &str) -> MatrixDefinition {
        serde_yaml_ng::from_str(source).unwrap()
    }

    #[test]
    fn cartesian_product_is_deterministic() {
        let matrix = definition("target: [linux-x64, windows-x64]\nprofile: [debug, release]\n");
        let combinations = expand(&matrix, 256).unwrap();
        assert_eq!(combinations.len(), 4);
        assert_eq!(combinations[0].suffix(), "[target=linux-x64,profile=debug]");
        assert_eq!(
            combinations[3].suffix(),
            "[target=windows-x64,profile=release]"
        );
    }

    #[test]
    fn include_merges_or_adds() {
        let matrix = definition(
            "os: [linux, windows]\ninclude:\n  - os: linux\n    arch: arm64\n  - os: macos\n",
        );
        let combinations = expand(&matrix, 256).unwrap();
        assert_eq!(combinations.len(), 3);
        assert!(combinations.iter().any(|c| c.values.get("arch").is_some()));
        assert!(combinations
            .iter()
            .any(|c| c.values.get("os") == Some(&serde_json::json!("macos"))));
    }

    #[test]
    fn exclude_removes_exact_matches() {
        let matrix = definition(
            "os: [linux, windows]\narch: [x64, arm64]\nexclude:\n  - os: linux\n    arch: arm64\n",
        );
        let combinations = expand(&matrix, 256).unwrap();
        assert_eq!(combinations.len(), 3);
        assert!(!combinations.iter().any(|c| {
            c.values.get("os") == Some(&serde_json::json!("linux"))
                && c.values.get("arch") == Some(&serde_json::json!("arm64"))
        }));
    }

    #[test]
    fn enforces_job_limit() {
        let matrix = definition("a: [1,2,3]\nb: [1,2,3]\nc: [1,2,3]\n");
        assert!(expand(&matrix, 10).is_err());
    }

    #[test]
    fn rejects_explosion_beyond_the_default_limit() {
        let values: Vec<String> = (0..DEFAULT_MAX_MATRIX_JOBS + 1)
            .map(|index| format!("v{index}"))
            .collect();
        let matrix = definition(&format!("axis: [{}]\n", values.join(", ")));
        let error = expand(&matrix, DEFAULT_MAX_MATRIX_JOBS).unwrap_err();
        assert_eq!(error.code, ErrorCode::ConfigInvalid);
        assert!(error.message.contains("beyond the limit"), "{error}");
    }
}
