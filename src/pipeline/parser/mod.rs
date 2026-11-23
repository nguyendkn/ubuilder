//! YAML parser boundary. The parser never executes or mutates anything.

use crate::error::{ErrorCode, Result, UbuilderError};
use crate::pipeline::schema::WorkflowDefinition;
use crate::pipeline::validator;
use std::path::{Path, PathBuf};

pub const MAX_WORKFLOW_BYTES: u64 = 1024 * 1024;
pub const DEFAULT_WORKFLOW_PATH: &str = ".ubuilder/workflows/build.yml";

#[derive(Debug, Clone)]
pub struct ParsedWorkflow {
    pub path: PathBuf,
    pub source: String,
    pub digest: String,
    pub definition: WorkflowDefinition,
}

impl ParsedWorkflow {
    pub fn load(path: &Path) -> Result<Self> {
        let metadata = std::fs::metadata(path).map_err(|error| {
            UbuilderError::new(
                ErrorCode::ConfigInvalid,
                format!("cannot read workflow {}: {error}", path.display()),
            )
        })?;
        if !metadata.is_file() {
            return Err(UbuilderError::new(
                ErrorCode::ConfigInvalid,
                format!("workflow path {} is not a file", path.display()),
            ));
        }
        if metadata.len() > MAX_WORKFLOW_BYTES {
            return Err(UbuilderError::new(
                ErrorCode::ConfigInvalid,
                format!(
                    "workflow {} is {} bytes; limit is {MAX_WORKFLOW_BYTES}",
                    path.display(),
                    metadata.len()
                ),
            ));
        }
        let source = std::fs::read_to_string(path).map_err(|error| {
            UbuilderError::new(
                ErrorCode::ConfigInvalid,
                format!("cannot read workflow {} as UTF-8: {error}", path.display()),
            )
        })?;
        Self::parse(path, source)
    }

    pub fn parse(path: &Path, source: String) -> Result<Self> {
        if source.len() as u64 > MAX_WORKFLOW_BYTES {
            return Err(UbuilderError::new(
                ErrorCode::ConfigInvalid,
                format!("workflow exceeds {MAX_WORKFLOW_BYTES} bytes"),
            ));
        }
        let definition: WorkflowDefinition =
            serde_yaml_ng::from_str(&source).map_err(|error| yaml_error(path, &error))?;
        let digest = crate::artifacts::sha256_bytes(source.as_bytes());
        Ok(Self {
            path: path.to_path_buf(),
            source,
            digest,
            definition,
        })
    }

    pub fn validate(&self) -> Result<()> {
        validator::validate(&self.definition, &self.path, &self.source)
    }

    /// Best-effort source line for a semantic diagnostic.
    pub fn line_of(&self, needle: &str) -> Option<usize> {
        self.source
            .lines()
            .position(|line| line.contains(needle))
            .map(|index| index + 1)
    }
}

/// Resolve the workflow path. Explicit paths must exist; the default searches
/// `.ubuilder/workflows/build.yml` then the single other YAML workflow.
pub fn resolve_path(root: &Path, explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            root.join(path)
        };
        if !resolved.is_file() {
            return Err(UbuilderError::new(
                ErrorCode::ConfigInvalid,
                format!("workflow file {} does not exist", resolved.display()),
            ));
        }
        return Ok(resolved);
    }
    let directory = root.join(".ubuilder").join("workflows");
    let default = directory.join("build.yml");
    if default.is_file() {
        return Ok(default);
    }
    let mut candidates = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&directory) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext == "yml" || ext == "yaml")
            {
                candidates.push(path);
            }
        }
    }
    candidates.sort();
    match candidates.len() {
        0 => Err(UbuilderError::new(
            ErrorCode::ConfigInvalid,
            format!(
                "no workflow found; expected {} (create one with 'ubuilder pipeline init')",
                default.display()
            ),
        )),
        1 => Ok(candidates.remove(0)),
        _ => Err(UbuilderError::new(
            ErrorCode::ConfigInvalid,
            format!(
                "multiple workflows found in {}; pass --file",
                directory.display()
            ),
        )),
    }
}

fn yaml_error(path: &Path, error: &serde_yaml_ng::Error) -> UbuilderError {
    let location = error.location();
    let where_ = match &location {
        Some(location) => format!(
            "{}:{}:{}",
            path.display(),
            location.line(),
            location.column()
        ),
        None => path.display().to_string(),
    };
    let mut details = serde_json::Map::new();
    details.insert("path".into(), serde_json::json!(path));
    if let Some(location) = location {
        details.insert("line".into(), serde_json::json!(location.line()));
        details.insert("column".into(), serde_json::json!(location.column()));
    }
    details.insert("yaml".into(), serde_json::json!(error.to_string()));
    let mut err = UbuilderError::new(
        ErrorCode::ConfigInvalid,
        format!("{where_}: invalid workflow YAML: {error}"),
    );
    err = err.details(serde_json::Value::Object(details));
    err
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path() -> PathBuf {
        PathBuf::from("workflow.yml")
    }

    #[test]
    fn parses_minimal_workflow() {
        let workflow = ParsedWorkflow::parse(
            &path(),
            "version: 1\nname: test\non:\n  manual: true\njobs:\n  build:\n    runs-on: auto\n    steps:\n      - run: echo hi\n"
                .into(),
        )
        .unwrap();
        assert_eq!(workflow.definition.name, "test");
        assert!(workflow.definition.triggers.manual);
        assert_eq!(workflow.definition.jobs.len(), 1);
    }

    #[test]
    fn reports_unknown_fields_with_location() {
        let error = ParsedWorkflow::parse(
            &path(),
            "version: 1\nname: test\nunexpected: true\njobs: {}\n".into(),
        )
        .unwrap_err();
        assert!(error.message.contains("unknown field"), "{}", error.message);
        assert!(
            error.message.contains("workflow.yml:3"),
            "{}",
            error.message
        );
    }

    #[test]
    fn rejects_duplicate_jobs() {
        let error = ParsedWorkflow::parse(
            &path(),
            "version: 1\nname: t\njobs:\n  a:\n    steps:\n      - run: echo a\n  a:\n    steps:\n      - run: echo b\n"
                .into(),
        )
        .unwrap_err();
        assert!(
            error.message.contains("duplicate job id"),
            "{}",
            error.message
        );
    }

    #[test]
    fn rejects_oversized_documents() {
        let source = format!("# {}\n", "x".repeat(MAX_WORKFLOW_BYTES as usize));
        assert!(ParsedWorkflow::parse(&path(), source).is_err());
    }
}
