use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriggerConfig {
    #[serde(default)]
    pub manual: bool,
    #[serde(default)]
    pub push: Option<BranchFilter>,
    #[serde(default, rename = "pull_request")]
    pub pull_request: Option<BranchFilter>,
    #[serde(default)]
    pub schedule: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BranchFilter {
    #[serde(default)]
    pub branches: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}
