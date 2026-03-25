//! types — extracted from report.rs.

use serde::Serialize;
use crate::output::{CreateOutput, EntityCrudOutput, MergeOutput, RemoveResult};
use super::super::{calculate_deploy_readiness, collect_status, list, load, Project};
use crate::error::Result;


#[derive(Debug, Clone, Serialize)]
pub struct ProjectListItem {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectComponentVersion {
    pub component_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_source: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectShowReport {
    pub project: Project,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    pub deploy_ready: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub deploy_blockers: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectListReport {
    pub projects: Vec<ProjectListItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectStatusReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<crate::server::health::ServerHealth>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_versions: Option<Vec<ProjectComponentVersion>>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct ProjectReportExtra {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projects: Option<Vec<ProjectListItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub components: Option<crate::project::ProjectComponentsOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin: Option<crate::project::ProjectPinOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removed: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deploy_ready: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deploy_blockers: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<crate::server::health::ServerHealth>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_versions: Option<Vec<ProjectComponentVersion>>,
}

pub type ProjectReportOutput = EntityCrudOutput<Project, ProjectReportExtra>;
