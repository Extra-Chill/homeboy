mod component_deploy_result;
mod deploy_result;
mod helpers;
mod release_state;
mod release_state_status;
mod types;

pub use component_deploy_result::*;
pub use deploy_result::*;
pub use helpers::*;
pub use release_state::*;
pub use release_state_status::*;
pub use types::*;

use serde::Serialize;

use crate::component::Component;
use crate::config;
use crate::error::Result;
use crate::is_zero_u32;
use crate::paths as base_path;

impl DeployResult {
    pub(super) fn success(exit_code: i32) -> Self {
        Self {
            success: true,
            exit_code,
            error: None,
        }
    }

    pub(super) fn failure(exit_code: i32, error: String) -> Self {
        Self {
            success: false,
            exit_code,
            error: Some(error),
        }
    }
}

impl ReleaseState {
    pub fn status(&self) -> ReleaseStateStatus {
        if self.has_uncommitted_changes {
            ReleaseStateStatus::Uncommitted
        } else if self.code_commits > 0 {
            ReleaseStateStatus::NeedsBump
        } else if self.docs_only_commits > 0 {
            ReleaseStateStatus::DocsOnly
        } else {
            ReleaseStateStatus::Clean
        }
    }
}

impl ReleaseStateStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ReleaseStateStatus::Uncommitted => "uncommitted",
            ReleaseStateStatus::NeedsBump => "needs_bump",
            ReleaseStateStatus::DocsOnly => "docs_only",
            ReleaseStateStatus::Clean => "clean",
            ReleaseStateStatus::Unknown => "unknown",
        }
    }
}

impl ComponentDeployResult {
    pub(super) fn new(component: &Component, base_path: &str) -> Self {
        Self {
            id: component.id.clone(),
            status: String::new(),
            deploy_reason: None,
            component_status: None,
            local_version: None,
            remote_version: None,
            error: None,
            artifact_path: component.build_artifact.clone(),
            remote_path: base_path::join_remote_path(Some(base_path), &component.remote_path).ok(),
            build_exit_code: None,
            deploy_exit_code: None,
            release_state: None,
            deployed_ref: None,
        }
    }

    /// Shorthand for the common failure pattern: status="failed" + versions + error.
    pub(super) fn failed(
        component: &Component,
        base_path: &str,
        local_version: Option<String>,
        remote_version: Option<String>,
        error: String,
    ) -> Self {
        Self::new(component, base_path)
            .with_status("failed")
            .with_versions(local_version, remote_version)
            .with_error(error)
    }

    pub(super) fn with_status(mut self, status: &str) -> Self {
        self.status = status.to_string();
        self
    }

    pub(super) fn with_versions(mut self, local: Option<String>, remote: Option<String>) -> Self {
        self.local_version = local;
        self.remote_version = remote;
        self
    }

    pub(super) fn with_error(mut self, error: String) -> Self {
        self.error = Some(error);
        self
    }

    pub(super) fn with_build_exit_code(mut self, code: Option<i32>) -> Self {
        self.build_exit_code = code;
        self
    }

    pub(super) fn with_deploy_exit_code(mut self, code: Option<i32>) -> Self {
        self.deploy_exit_code = code;
        self
    }

    pub(super) fn with_component_status(mut self, status: ComponentStatus) -> Self {
        self.component_status = Some(status);
        self
    }

    pub(super) fn with_remote_path(mut self, path: String) -> Self {
        self.remote_path = Some(path);
        self
    }

    pub(super) fn with_release_state(mut self, state: ReleaseState) -> Self {
        self.release_state = Some(state);
        self
    }

    pub(super) fn with_deployed_ref(mut self, git_ref: String) -> Self {
        self.deployed_ref = Some(git_ref);
        self
    }
}
