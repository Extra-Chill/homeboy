//! component_deploy_result — extracted from types.rs.

use serde::Serialize;
use crate::component::Component;
use crate::error::Result;
use super::DeployReason;
use super::ComponentStatus;
use super::ReleaseState;


/// Result for a single component deployment.
#[derive(Debug, Clone, Serialize)]

pub struct ComponentDeployResult {
    pub id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deploy_reason: Option<DeployReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_status: Option<ComponentStatus>,
    pub local_version: Option<String>,
    pub remote_version: Option<String>,
    pub error: Option<String>,
    pub artifact_path: Option<String>,
    pub remote_path: Option<String>,
    pub build_exit_code: Option<i32>,
    pub deploy_exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_state: Option<ReleaseState>,
    /// The git ref (tag or branch) that was built and deployed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployed_ref: Option<String>,
}
