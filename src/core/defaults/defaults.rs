//! defaults — extracted from defaults.rs.

use serde::{Deserialize, Serialize};
use super::VersionCandidateConfig;
use super::PermissionsConfig;
use super::InstallMethodsConfig;
use super::DeployConfig;


/// All configurable defaults that can be overridden via homeboy.json
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Defaults {
    #[serde(default = "default_install_methods")]
    pub install_methods: InstallMethodsConfig,

    #[serde(default = "default_version_candidates")]
    pub version_candidates: Vec<VersionCandidateConfig>,

    #[serde(default = "default_deploy")]
    pub deploy: DeployConfig,

    #[serde(default = "default_permissions")]
    pub permissions: PermissionsConfig,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            install_methods: default_install_methods(),
            version_candidates: default_version_candidates(),
            deploy: default_deploy(),
            permissions: default_permissions(),
        }
    }
}
