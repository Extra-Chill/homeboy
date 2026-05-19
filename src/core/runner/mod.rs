use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::config::{self, ConfigEntity};
use crate::core::error::{Error, Result};
use crate::core::output::{CreateOutput, MergeOutput, RemoveResult};
use crate::core::server;

mod apply;
mod connection;
mod evidence;
mod execution;
mod workspace;

pub use apply::{
    apply_workspace_patch, RunnerWorkspaceApplyOptions, RunnerWorkspaceApplyOutput,
    RunnerWorkspaceApplyStatus,
};
pub use connection::{
    connect, disconnect, status, RunnerConnectReport, RunnerDisconnectReport, RunnerFailureKind,
    RunnerSession, RunnerStatusReport,
};
pub use evidence::{
    download_remote_artifact, is_remote_runner_artifact_path, RemoteArtifactDownload,
};
pub(crate) use execution::daemon_api_get;
pub use execution::{exec, RunnerExecMode, RunnerExecOptions, RunnerExecOutput};
pub use workspace::{
    sync_workspace, RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions, RunnerWorkspaceSyncOutput,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerKind {
    Local,
    Ssh,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Runner {
    #[serde(skip_deserializing, default)]
    pub id: String,
    pub kind: RunnerKind,
    #[serde(default)]
    pub server_id: Option<String>,
    #[serde(default)]
    pub workspace_root: Option<String>,
    #[serde(default)]
    pub homeboy_path: Option<String>,
    #[serde(default)]
    pub daemon: bool,
    #[serde(default)]
    pub concurrency_limit: Option<usize>,
    #[serde(default)]
    pub artifact_policy: Option<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub resources: HashMap<String, Value>,
}

impl ConfigEntity for Runner {
    const ENTITY_TYPE: &'static str = "runner";
    const DIR_NAME: &'static str = "runners";

    fn id(&self) -> &str {
        &self.id
    }

    fn set_id(&mut self, id: String) {
        self.id = id;
    }

    fn not_found_error(id: String, suggestions: Vec<String>) -> Error {
        Error::runner_not_found(id, suggestions)
    }

    fn validate(&self) -> Result<()> {
        if matches!(self.kind, RunnerKind::Ssh) {
            let server_id = self.server_id.as_deref().ok_or_else(|| {
                Error::validation_invalid_argument(
                    "server_id",
                    "SSH runners require server_id",
                    None,
                    None,
                )
            })?;
            server::load(server_id)?;
        }

        if self.concurrency_limit == Some(0) {
            return Err(Error::validation_invalid_argument(
                "concurrency_limit",
                "concurrency_limit must be greater than zero",
                None,
                None,
            ));
        }

        Ok(())
    }

    fn dependents(_id: &str) -> Result<Vec<String>> {
        Ok(vec![])
    }
}

entity_crud!(Runner; merge);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support;

    #[test]
    fn runner_registry_persists_local_runner() {
        test_support::with_isolated_home(|_| {
            let spec = r#"{
                "id": "lab-local",
                "kind": "local",
                "workspace_root": "/Users/chubes/Developer",
                "homeboy_path": "/usr/local/bin/homeboy",
                "daemon": true,
                "concurrency_limit": 2,
                "artifact_policy": "copy",
                "env": {"RUST_LOG": "info"},
                "resources": {"cpu": 8}
            }"#;

            create(spec, false).expect("create runner");
            let runner = load("lab-local").expect("load runner");

            assert_eq!(runner.id, "lab-local");
            assert_eq!(runner.kind, RunnerKind::Local);
            assert_eq!(runner.server_id, None);
            assert_eq!(
                runner.workspace_root.as_deref(),
                Some("/Users/chubes/Developer")
            );
            assert_eq!(runner.concurrency_limit, Some(2));
            assert_eq!(runner.env.get("RUST_LOG").map(String::as_str), Some("info"));
            assert_eq!(runner.resources.get("cpu"), Some(&Value::from(8)));
        });
    }

    #[test]
    fn ssh_runner_requires_existing_server() {
        test_support::with_isolated_home(|_| {
            let spec = r#"{
                "id": "remote-lab",
                "kind": "ssh",
                "server_id": "missing",
                "workspace_root": "/srv/homeboy"
            }"#;

            let err = create(spec, false).expect_err("missing server rejects ssh runner");
            assert_eq!(err.code.as_str(), "server.not_found");
        });
    }

    #[test]
    fn runner_set_updates_fields() {
        test_support::with_isolated_home(|_| {
            create(
                r#"{"id":"lab-local","kind":"local","workspace_root":"/tmp/a"}"#,
                false,
            )
            .expect("create runner");

            let result = merge(
                Some("lab-local"),
                r#"{"workspace_root":"/tmp/b","concurrency_limit":3}"#,
                &[],
            )
            .expect("merge runner");

            match result {
                MergeOutput::Single(result) => {
                    assert_eq!(result.id, "lab-local");
                    assert!(result
                        .updated_fields
                        .contains(&"workspace_root".to_string()));
                    assert!(result
                        .updated_fields
                        .contains(&"concurrency_limit".to_string()));
                }
                MergeOutput::Bulk(_) => panic!("expected single merge"),
            }

            let runner = load("lab-local").expect("load runner");
            assert_eq!(runner.workspace_root.as_deref(), Some("/tmp/b"));
            assert_eq!(runner.concurrency_limit, Some(3));
        });
    }
}
