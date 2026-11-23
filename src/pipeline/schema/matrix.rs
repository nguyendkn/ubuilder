use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MatrixStrategy {
    #[serde(rename = "fail-fast", default = "default_true")]
    pub fail_fast: bool,
    #[serde(rename = "max-parallel", default)]
    pub max_parallel: Option<usize>,
    pub matrix: MatrixDefinition,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MatrixDefinition {
    #[serde(flatten)]
    pub axes: IndexMap<String, Vec<serde_yaml_ng::Value>>,
    #[serde(default)]
    pub include: Vec<IndexMap<String, serde_yaml_ng::Value>>,
    #[serde(default)]
    pub exclude: Vec<IndexMap<String, serde_yaml_ng::Value>>,
}

impl MatrixDefinition {
    pub fn is_empty(&self) -> bool {
        self.axes.is_empty() && self.include.is_empty()
    }
}
