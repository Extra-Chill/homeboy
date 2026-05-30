use serde::{Deserialize, Serialize};

fn default_staging_path() -> String {
    "/tmp/homeboy-staging".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployArchiveInstallPolicy {
    pub path_pattern: String,
    #[serde(default = "default_staging_path")]
    pub staging_path: String,
    #[serde(default)]
    pub root_must_match_target_basename: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_header: Option<DeployRequiredHeader>,
    #[serde(default)]
    pub skip_permissions_fix: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployRequiredHeader {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_glob: Option<String>,
    pub contains: String,
}
