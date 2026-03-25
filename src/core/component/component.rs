//! component — extracted from mod.rs.

use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use crate::core::component::GitDeployConfig;
use crate::core::component::ScopeConfig;
use crate::core::component::ScopedExtensionConfig;
use crate::core::component::VersionTarget;


#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(from = "RawComponent", into = "RawComponent")]
pub struct Component {
    pub id: String,
    pub aliases: Vec<String>,
    pub local_path: String,
    pub remote_path: String,
    pub build_artifact: Option<String>,
    pub extensions: Option<HashMap<String, ScopedExtensionConfig>>,
    pub version_targets: Option<Vec<VersionTarget>>,
    pub changelog_target: Option<String>,
    pub changelog_next_section_label: Option<String>,
    pub changelog_next_section_aliases: Option<Vec<String>>,
    /// Lifecycle hooks: event name -> list of shell commands.
    /// Events: `pre:version:bump`, `post:version:bump`, `post:release`, `post:deploy`
    pub hooks: HashMap<String, Vec<String>>,
    pub extract_command: Option<String>,
    pub remote_owner: Option<String>,
    pub deploy_strategy: Option<String>,
    pub git_deploy: Option<GitDeployConfig>,
    /// Git remote URL for the component's source repository (e.g., GitHub URL).
    /// Used by deploy to download release artifacts or initialize server-side git repos.
    pub remote_url: Option<String>,
    pub auto_cleanup: bool,
    pub docs_dir: Option<String>,
    pub docs_dirs: Vec<String>,
    pub scopes: Option<ScopeConfig>,
}
