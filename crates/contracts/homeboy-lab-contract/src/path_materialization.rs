//! Path materialization plan types.
//!
//! Self-contained data types describing how a runner materializes workspace
//! paths (existing-remote, git, snapshot) plus their projections. Extracted into
//! this leaf module (from `runner_execution_envelope`) so the lab-contract type
//! layer can hold a `PathMaterializationPlan` field without depending on the
//! wider runner-execution envelope machinery. `runner_execution_envelope`
//! re-exports these for its existing call sites.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

pub const PATH_MATERIALIZATION_PLAN_SCHEMA: &str = "homeboy/path-materialization-plan/v1";
pub const PATH_MATERIALIZATION_MODE_EXISTING_REMOTE: &str = "existing_remote";
pub const PATH_MATERIALIZATION_MODE_GIT: &str = "git";
pub const PATH_MATERIALIZATION_MODE_SNAPSHOT: &str = "snapshot";
pub const PATH_MATERIALIZATION_ROLE_PRIMARY_WORKSPACE: &str = "primary_workspace";
pub const PATH_MATERIALIZATION_ROLE_REQUIRED_PATH: &str = "required_path";
pub const PATH_MATERIALIZATION_OWNER_RUNNER_EXEC_SOURCE_SNAPSHOT: &str =
    "runner_exec.source_snapshot";
pub const PATH_MATERIALIZATION_OWNER_RUNNER_EXEC_REQUIRE_PATHS: &str = "runner_exec.require_paths";
pub const PATH_MATERIALIZATION_OWNER_LAB_EXECUTION_CONTEXT: &str = "lab.execution_context";
pub const PATH_MATERIALIZATION_OWNER_LAB_PROVIDER_CONFIG: &str = "lab.provider_config";
pub const PATH_MATERIALIZATION_STATUS_MATERIALIZED: &str = "materialized";
pub const PATH_MATERIALIZATION_STATUS_VALIDATED: &str = "validated";

fn path_materialization_plan_schema() -> String {
    PATH_MATERIALIZATION_PLAN_SCHEMA.to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathMaterializationMode {
    ExistingRemote,
    Git,
    Snapshot,
}

impl PathMaterializationMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExistingRemote => PATH_MATERIALIZATION_MODE_EXISTING_REMOTE,
            Self::Git => PATH_MATERIALIZATION_MODE_GIT,
            Self::Snapshot => PATH_MATERIALIZATION_MODE_SNAPSHOT,
        }
    }
}

impl fmt::Display for PathMaterializationMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for PathMaterializationMode {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            PATH_MATERIALIZATION_MODE_EXISTING_REMOTE => Ok(Self::ExistingRemote),
            PATH_MATERIALIZATION_MODE_GIT => Ok(Self::Git),
            PATH_MATERIALIZATION_MODE_SNAPSHOT => Ok(Self::Snapshot),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PathMaterializationProjection {
    pub role: String,
    pub owner: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_path: Option<String>,
    pub remote_path: String,
    pub materialization_mode: String,
    pub validation_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PathMaterializationPlanProjection {
    pub schema: String,
    pub entries: Vec<PathMaterializationProjection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path_remaps: Vec<PathMaterializationPathRemap>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PathMaterializationPathRemap {
    pub local_path: String,
    pub remote_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PathMaterializationPlan {
    #[serde(default = "path_materialization_plan_schema")]
    pub schema: String,
    pub entries: Vec<PathMaterializationEntry>,
}

impl PathMaterializationPlan {
    pub fn new(entries: impl IntoIterator<Item = PathMaterializationEntry>) -> Self {
        Self {
            schema: PATH_MATERIALIZATION_PLAN_SCHEMA.to_string(),
            entries: entries.into_iter().collect(),
        }
    }

    pub fn non_empty(entries: impl IntoIterator<Item = PathMaterializationEntry>) -> Option<Self> {
        let plan = Self::new(entries);
        if plan.entries.is_empty() {
            None
        } else {
            Some(plan)
        }
    }

    pub fn projection_entries(&self) -> Vec<PathMaterializationProjection> {
        self.entries
            .iter()
            .map(PathMaterializationProjection::from)
            .collect()
    }

    pub fn path_remaps(&self) -> Vec<PathMaterializationPathRemap> {
        self.entries
            .iter()
            .filter_map(PathMaterializationPathRemap::from_entry)
            .collect()
    }

    pub fn mapping_ref(&self) -> Option<&'static str> {
        (!self.entries.is_empty()).then_some("path_materialization_plan")
    }

    pub fn projection(&self) -> PathMaterializationPlanProjection {
        PathMaterializationPlanProjection {
            schema: self.schema.clone(),
            entries: self.projection_entries(),
            path_remaps: self.path_remaps(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PathMaterializationEntry {
    pub role: String,
    pub owner: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_path: Option<String>,
    pub remote_path: String,
    pub materialization_mode: String,
    pub validation_status: String,
}

impl PathMaterializationEntry {
    pub fn new(
        role: impl Into<String>,
        owner: impl Into<String>,
        local_path: Option<String>,
        remote_path: impl Into<String>,
        materialization_mode: impl Into<String>,
        validation_status: impl Into<String>,
    ) -> Self {
        Self {
            role: role.into(),
            owner: owner.into(),
            local_path,
            remote_path: remote_path.into(),
            materialization_mode: materialization_mode.into(),
            validation_status: validation_status.into(),
        }
    }

    pub fn with_mode(
        role: impl Into<String>,
        owner: impl Into<String>,
        local_path: Option<String>,
        remote_path: impl Into<String>,
        materialization_mode: PathMaterializationMode,
        validation_status: impl Into<String>,
    ) -> Self {
        Self::new(
            role,
            owner,
            local_path,
            remote_path,
            materialization_mode.to_string(),
            validation_status,
        )
    }

    pub fn required_existing_remote(remote_path: impl Into<String>) -> Self {
        Self::with_mode(
            PATH_MATERIALIZATION_ROLE_REQUIRED_PATH,
            PATH_MATERIALIZATION_OWNER_RUNNER_EXEC_REQUIRE_PATHS,
            None,
            remote_path,
            PathMaterializationMode::ExistingRemote,
            PATH_MATERIALIZATION_STATUS_VALIDATED,
        )
    }

    pub fn primary_workspace_materialized(
        owner: impl Into<String>,
        local_path: Option<String>,
        remote_path: impl Into<String>,
        materialization_mode: impl Into<String>,
    ) -> Self {
        Self::new(
            PATH_MATERIALIZATION_ROLE_PRIMARY_WORKSPACE,
            owner,
            local_path,
            remote_path,
            materialization_mode,
            PATH_MATERIALIZATION_STATUS_MATERIALIZED,
        )
    }

    pub fn primary_workspace_existing_remote(remote_path: impl Into<String>) -> Self {
        Self::new(
            PATH_MATERIALIZATION_ROLE_PRIMARY_WORKSPACE,
            PATH_MATERIALIZATION_OWNER_LAB_EXECUTION_CONTEXT,
            None,
            remote_path,
            PATH_MATERIALIZATION_MODE_EXISTING_REMOTE,
            PATH_MATERIALIZATION_STATUS_VALIDATED,
        )
    }

    pub fn mode(&self) -> Option<PathMaterializationMode> {
        PathMaterializationMode::from_str(&self.materialization_mode).ok()
    }
}

impl From<&PathMaterializationEntry> for PathMaterializationProjection {
    fn from(entry: &PathMaterializationEntry) -> Self {
        Self {
            role: entry.role.clone(),
            owner: entry.owner.clone(),
            local_path: entry.local_path.clone(),
            remote_path: entry.remote_path.clone(),
            materialization_mode: entry.materialization_mode.clone(),
            validation_status: entry.validation_status.clone(),
        }
    }
}

impl PathMaterializationPathRemap {
    pub fn from_entry(entry: &PathMaterializationEntry) -> Option<Self> {
        let local_path = entry.local_path.as_deref()?.trim();
        let remote_path = entry.remote_path.trim();
        if local_path.is_empty() || remote_path.is_empty() {
            return None;
        }

        Some(Self {
            local_path: local_path.to_string(),
            remote_path: remote_path.to_string(),
        })
    }
}
