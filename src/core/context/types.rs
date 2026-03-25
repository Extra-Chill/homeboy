//! types — extracted from mod.rs.

use serde::Serialize;
use crate::project::{self, Project};
use crate::server::SshClient;
use crate::server::{self, Server};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use crate::error::{Error, Result};


#[derive(Debug, Clone, Serialize)]

pub struct ComponentGap {
    pub field: String,
    pub reason: String,
    pub command: String,
}

#[derive(Debug, Clone, Serialize)]

pub struct ContainedComponentInfo {
    pub id: String,
    pub build_artifact: String,
    pub remote_path: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<ComponentGap>,
}

#[derive(Debug, Clone, Serialize)]

pub struct ProjectContext {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
}

#[derive(Debug, Clone, Serialize)]

pub struct ContextOutput {
    #[serde(skip_serializing)]
    pub command: String,
    pub cwd: String,
    pub git_root: Option<String>,
    pub managed: bool,
    pub matched_components: Vec<String>,
    #[serde(skip_serializing)]
    pub contained_components: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

pub(crate) struct ProjectServerContext {
    pub project: Project,
    pub server_id: String,
    pub server: Server,
}

pub struct RemoteProjectContext {
    pub project: Project,
    pub server_id: String,
    pub server: Server,
    pub client: SshClient,
    pub base_path: Option<String>,
}
