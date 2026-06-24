use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectComponentAttachment {
    pub id: String,
    pub local_path: String,
    /// Project-specific deploy target for this attached component.
    ///
    /// Repo-owned `homeboy.json` is portable component metadata, while the
    /// install path can vary by project layout. Keeping this optional field on
    /// the attachment lets one component deploy to multiple projects without
    /// rewriting the repo-tracked `remote_path` for each environment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectComponentOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_artifact: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extract_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deploy_strategy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_deploy: Option<crate::core::component::GitDeployConfig>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub hooks: HashMap<String, Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scopes: Option<crate::core::component::ScopeConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_inputs: Vec<crate::core::component::ArtifactInput>,
    /// Override the CLI path used by extension deploy install steps.
    /// For example, local wrappers may need "lando wp" instead of the default "wp".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli_path: Option<String>,
}
