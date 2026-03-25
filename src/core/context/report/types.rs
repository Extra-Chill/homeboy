//! types — extracted from report.rs.

use std::collections::{HashMap, HashSet};
use serde::Serialize;
use crate::component::{self, Component};
use crate::deploy;
use crate::extension::{
    extension_ready_status, is_extension_compatible, is_extension_linked, load_all_extensions,
};
use crate::project::{self, Project};
use crate::server::{self, Server};
use crate::{changelog, git, is_zero, is_zero_u32, version, Result};
use super::super::{build_component_info, path_is_parent_of, ComponentGap, ContextOutput};
use std::path::{Path, PathBuf};


#[derive(Debug, Serialize)]
pub struct GapSummary {
    pub component_id: String,
    pub field: String,
    pub reason: String,
    pub command: String,
}

#[derive(Debug, Serialize)]
pub struct ContextReportStatus {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ready_to_deploy: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub needs_version_bump: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub docs_only: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub has_uncommitted: Vec<String>,
    #[serde(skip_serializing_if = "is_zero")]
    pub config_gaps: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub gap_details: Vec<GapSummary>,
}

#[derive(Debug, Serialize)]
pub struct ContextReportSummary {
    pub total_components: usize,
    pub by_extension: HashMap<String, usize>,
    pub by_status: HashMap<String, usize>,
}

#[derive(Debug, Serialize)]
pub struct ComponentSummary {
    pub id: String,
    pub path: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
    pub status: String,
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub commits_since_version: u32,
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub code_commits: u32,
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub docs_only_commits: u32,
}

#[derive(Debug, Serialize)]
pub struct ContextReport {
    pub command: String,
    pub status: ContextReportStatus,
    pub summary: ContextReportSummary,
    pub context: ContextOutput,
    pub next_steps: Vec<String>,
    pub components: Vec<ComponentSummary>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub servers: Vec<Server>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub projects: Vec<ProjectListItem>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<ExtensionEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<VersionSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git: Option<GitSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_release: Option<ReleaseSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changelog: Option<ChangelogSnapshot>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub agent_context_files: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ProjectListItem {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sub_targets: Vec<String>,
}

impl From<Project> for ProjectListItem {
    fn from(p: Project) -> Self {
        Self {
            id: p.id.clone(),
            domain: p.domain,
            sub_targets: p
                .sub_targets
                .iter()
                .filter_map(|st| project::slugify_id(&st.name).ok())
                .collect(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ExtensionEntry {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub runtime: String,
    pub compatible: bool,
    pub ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_detail: Option<String>,
    pub linked: bool,
}

#[derive(Debug, Serialize)]
pub struct VersionSnapshot {
    pub component_id: String,
    pub version: String,
    pub targets: Vec<version::VersionTargetInfo>,
}

#[derive(Debug, Serialize)]
pub struct GitSnapshot {
    pub branch: String,
    pub clean: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ahead: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behind: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commits_since_version: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_warning: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ReleaseSnapshot {
    pub tag: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ChangelogSnapshot {
    pub path: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<String>>,
}

pub type ComponentReleaseState = crate::deploy::ReleaseState;

#[derive(Debug, Clone, Serialize)]
pub struct ComponentWithState {
    #[serde(flatten)]
    pub component: Component,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_state: Option<ComponentReleaseState>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<ComponentGap>,
}
