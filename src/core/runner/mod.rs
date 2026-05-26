use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::config::{self, ConfigEntity};
use crate::core::defaults;
use crate::core::error::{Error, Result};
use crate::core::output::{BatchResult, CreateOutput, CreateResult, MergeOutput, MergeResult};
use crate::core::server::{self, RunnerSettings, ServerRunner};

mod apply;
mod connection;
mod evidence;
mod execution;
mod offload_changed_since;
mod offload_metadata;
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
    download_remote_artifact, is_remote_runner_artifact_path, is_reportable_artifact_evidence_path,
    is_retrievable_runner_artifact, reportable_artifact_evidence_path, RemoteArtifactDownload,
};
pub(crate) use execution::daemon_api_get;
pub use execution::{exec, RunnerExecMode, RunnerExecOptions, RunnerExecOutput};
pub use offload_changed_since::{
    lab_offload_changed_since_ref, preflight_lab_offload_changed_since,
    prepare_git_lab_offload_changed_since,
};
pub use offload_metadata::{capture_lab_offload_metadata, lab_offload_metadata};
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
    #[serde(flatten)]
    pub settings: RunnerSettings,
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

        if self.settings.concurrency_limit == Some(0) {
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

pub fn load(id: &str) -> Result<Runner> {
    if config::exists::<Runner>(id) {
        return config::load::<Runner>(id);
    }

    load_server_runner(id)
}

pub fn list() -> Result<Vec<Runner>> {
    let mut runners = config::list::<Runner>()?;
    runners.extend(
        server::list()?
            .into_iter()
            .filter(|server| server.runner.is_some())
            .map(|server| runner_from_server(&server.id, server.runner.expect("checked above"))),
    );
    runners.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(runners)
}

pub fn resolve_default_lab_runner() -> Result<Option<String>> {
    let preferred = defaults::load_config().lab.preferred_runner;
    let runners = list()?;
    Ok(resolve_default_lab_runner_from_candidates(
        preferred.as_deref(),
        runners.into_iter().filter_map(|runner| {
            if runner.kind != RunnerKind::Ssh {
                return None;
            }
            let connected = status(&runner.id).ok()?.connected;
            Some(DefaultLabRunnerCandidate {
                id: runner.id,
                connected,
            })
        }),
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DefaultLabRunnerCandidate {
    id: String,
    connected: bool,
}

fn resolve_default_lab_runner_from_candidates(
    preferred: Option<&str>,
    candidates: impl IntoIterator<Item = DefaultLabRunnerCandidate>,
) -> Option<String> {
    let candidates: Vec<DefaultLabRunnerCandidate> = candidates.into_iter().collect();

    if let Some(preferred) = preferred {
        return candidates
            .into_iter()
            .find(|candidate| candidate.id == preferred)
            .map(|candidate| candidate.id);
    }

    if candidates.len() == 1 {
        return candidates.into_iter().next().map(|candidate| candidate.id);
    }

    let connected: Vec<DefaultLabRunnerCandidate> = candidates
        .into_iter()
        .filter(|candidate| candidate.connected)
        .collect();

    if connected.len() == 1 {
        connected.into_iter().next().map(|candidate| candidate.id)
    } else {
        None
    }
}

pub fn create(json_spec: &str, skip_existing: bool) -> Result<CreateOutput<Runner>> {
    let raw = config::read_json_spec_to_string(json_spec)?;
    let value: Value = config::from_str(&raw)?;

    if let Some(items) = value.as_array() {
        let mut summary = BatchResult::new();
        for item in items {
            let id = item
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            if skip_existing && load(&id).is_ok() {
                summary.record_skipped(id);
                continue;
            }

            match create_single_value(item.clone()) {
                Ok(result) => summary.record_created(result.id),
                Err(err) => summary.record_error(id, err.message),
            }
        }
        return Ok(CreateOutput::Bulk(summary));
    }

    Ok(CreateOutput::Single(create_single_value(value)?))
}

pub fn merge(id: Option<&str>, json_spec: &str, replace_fields: &[String]) -> Result<MergeOutput> {
    let raw = config::read_json_spec_to_string(json_spec)?;
    let parsed: Value = config::from_str(&raw)?;

    if parsed.is_array() {
        return Ok(MergeOutput::Bulk(config::merge_batch_from_json::<Runner>(
            &raw,
        )?));
    }

    let effective_id = id
        .map(String::from)
        .or_else(|| parsed.get("id").and_then(Value::as_str).map(String::from))
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "id",
                "Provide runner ID as argument or in JSON body",
                None,
                None,
            )
        })?;

    if config::exists::<Runner>(&effective_id) {
        return Ok(MergeOutput::Single(config::merge_from_json::<Runner>(
            Some(&effective_id),
            &raw,
            replace_fields,
        )?));
    }

    Ok(MergeOutput::Single(merge_server_runner(
        &effective_id,
        parsed,
        replace_fields,
    )?))
}

pub fn delete_safe(id: &str) -> Result<()> {
    if config::exists::<Runner>(id) {
        return config::delete_safe::<Runner>(id);
    }

    let mut server = server::load(id)?;
    if server.runner.is_none() {
        return Err(Error::runner_not_found(id.to_string(), vec![]));
    }
    server.runner = None;
    server::save(&server)
}

pub fn exists(id: &str) -> bool {
    config::exists::<Runner>(id) || load_server_runner(id).is_ok()
}

pub fn enable_server_runner(server_id: &str, patch: Value) -> Result<Runner> {
    let mut server = server::load(server_id)?;
    let mut runner = server.runner.unwrap_or_default();
    let patch = strip_runner_identity_fields(patch);
    if !matches!(patch.as_object(), Some(obj) if obj.is_empty()) {
        config::merge_config(&mut runner, patch, &[])?;
    }
    validate_server_runner(server_id, &runner)?;
    server.runner = Some(runner.clone());
    server::save(&server)?;
    Ok(runner_from_server(server_id, runner))
}

pub fn migrate_standalone_ssh_runner(id: &str, remove_legacy: bool) -> Result<Runner> {
    let runner = config::load::<Runner>(id)?;
    if !matches!(runner.kind, RunnerKind::Ssh) {
        return Err(Error::validation_invalid_argument(
            "runner_id",
            "Only standalone SSH runners can be migrated to server capabilities",
            Some(id.to_string()),
            None,
        ));
    }

    let server_id = runner.server_id.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "server_id",
            "SSH runners require server_id before migration",
            Some(id.to_string()),
            None,
        )
    })?;
    if server_id == id {
        return Err(Error::validation_invalid_argument(
            "runner_id",
            "Runner already uses its server ID; no migration is needed",
            Some(id.to_string()),
            None,
        ));
    }

    let mut server = server::load(server_id)?;
    let mut server_runner = server.runner.unwrap_or_default();
    server_runner.workspace_root = runner.workspace_root.clone();
    server_runner.settings = runner.settings.clone();
    server_runner.env.extend(runner.env.clone());
    server_runner.resources.extend(runner.resources.clone());
    validate_server_runner(server_id, &server_runner)?;
    server.runner = Some(server_runner.clone());
    server::save(&server)?;

    if remove_legacy {
        config::delete_safe::<Runner>(id)?;
    }

    Ok(runner_from_server(server_id, server_runner))
}

fn create_single_value(value: Value) -> Result<CreateResult<Runner>> {
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Error::validation_invalid_argument("id", "Missing required field: id", None, None)
        })?
        .to_string();
    let mut runner: Runner = serde_json::from_value(value.clone())
        .map_err(|err| Error::validation_invalid_argument("json", err.to_string(), None, None))?;
    runner.set_id(id.clone());

    match runner.kind {
        RunnerKind::Local => {
            if config::exists::<Runner>(&id) {
                return Err(Error::validation_invalid_argument(
                    "runner.id",
                    format!("runner '{}' already exists", id),
                    Some(id),
                    None,
                ));
            }
            runner.validate()?;
            config::save(&runner)?;
            Ok(CreateResult {
                id: runner.id.clone(),
                entity: runner,
            })
        }
        RunnerKind::Ssh => {
            let server_id = runner.server_id.as_deref().unwrap_or(&id);
            if server_id != id {
                return Err(Error::validation_invalid_argument(
                    "server_id",
                    "SSH runner IDs are server IDs; use the server ID as the runner ID",
                    Some(server_id.to_string()),
                    Some(vec![format!(
                        "Run `homeboy runner enable {server_id}` to make server '{server_id}' runner-capable."
                    )]),
                ));
            }
            let entity = enable_server_runner(&id, value)?;
            Ok(CreateResult { id, entity })
        }
    }
}

fn load_server_runner(id: &str) -> Result<Runner> {
    let server = server::load(id)?;
    let runner = server
        .runner
        .ok_or_else(|| Error::runner_not_found(id.to_string(), vec![]))?;
    Ok(runner_from_server(id, runner))
}

fn runner_from_server(server_id: &str, runner: ServerRunner) -> Runner {
    Runner {
        id: server_id.to_string(),
        kind: RunnerKind::Ssh,
        server_id: Some(server_id.to_string()),
        workspace_root: runner.workspace_root,
        settings: runner.settings,
        env: runner.env,
        resources: runner.resources,
    }
}

fn merge_server_runner(
    id: &str,
    mut patch: Value,
    replace_fields: &[String],
) -> Result<MergeResult> {
    let mut server = server::load(id)?;
    let mut runner = server.runner.unwrap_or_default();
    if let Some(obj) = patch.as_object_mut() {
        obj.remove("id");
        obj.remove("kind");
        obj.remove("server_id");
    }
    let result = config::merge_config(&mut runner, patch, replace_fields)?;
    validate_server_runner(id, &runner)?;
    server.runner = Some(runner);
    server::save(&server)?;
    Ok(MergeResult {
        id: id.to_string(),
        updated_fields: result.updated_fields,
    })
}

fn strip_runner_identity_fields(mut value: Value) -> Value {
    if let Some(obj) = value.as_object_mut() {
        obj.remove("id");
        obj.remove("kind");
        obj.remove("server_id");
    }
    value
}

fn validate_server_runner(server_id: &str, runner: &ServerRunner) -> Result<()> {
    if runner.settings.concurrency_limit == Some(0) {
        return Err(Error::validation_invalid_argument(
            "concurrency_limit",
            "concurrency_limit must be greater than zero",
            Some(server_id.to_string()),
            None,
        ));
    }
    Ok(())
}

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
            assert_eq!(runner.settings.concurrency_limit, Some(2));
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
                "server_id": "remote-lab",
                "workspace_root": "/srv/homeboy"
            }"#;

            let err = create(spec, false).expect_err("missing server rejects ssh runner");
            assert_eq!(err.code.as_str(), "server.not_found");
        });
    }

    #[test]
    fn ssh_runner_is_server_capability() {
        test_support::with_isolated_home(|_| {
            server::create(
                r#"{"id":"homeboy-lab","host":"192.168.86.63","user":"chubes"}"#,
                false,
            )
            .expect("create server");

            create(
                r#"{
                    "id":"homeboy-lab",
                    "kind":"ssh",
                    "server_id":"homeboy-lab",
                    "workspace_root":"/home/chubes/Developer",
                    "concurrency_limit":4,
                    "artifact_policy":"copy"
                }"#,
                false,
            )
            .expect("enable runner capability");

            let runner = load("homeboy-lab").expect("load server runner");
            assert_eq!(runner.id, "homeboy-lab");
            assert_eq!(runner.kind, RunnerKind::Ssh);
            assert_eq!(runner.server_id.as_deref(), Some("homeboy-lab"));
            assert_eq!(
                runner.workspace_root.as_deref(),
                Some("/home/chubes/Developer")
            );
            assert_eq!(runner.settings.concurrency_limit, Some(4));

            let stored_server = server::load("homeboy-lab").expect("load server");
            assert!(stored_server.runner.is_some());
        });
    }

    #[test]
    fn ssh_runner_id_must_match_server_id() {
        test_support::with_isolated_home(|_| {
            server::create(
                r#"{"id":"homeboy-lab","host":"192.168.86.63","user":"chubes"}"#,
                false,
            )
            .expect("create server");

            let err = create(
                r#"{
                    "id":"lab",
                    "kind":"ssh",
                    "server_id":"homeboy-lab",
                    "workspace_root":"/home/chubes/Developer"
                }"#,
                false,
            )
            .expect_err("ssh runner cannot use a second ID");

            assert_eq!(err.code.as_str(), "validation.invalid_argument");
            assert!(err.message.contains("SSH runner IDs are server IDs"));
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
            assert_eq!(runner.settings.concurrency_limit, Some(3));
        });
    }

    #[test]
    fn migrates_standalone_ssh_runner_to_server_capability() {
        test_support::with_isolated_home(|_| {
            server::create(
                r#"{"id":"homeboy-lab","host":"192.168.86.63","user":"chubes"}"#,
                false,
            )
            .expect("create server");

            let legacy = Runner {
                id: "lab".to_string(),
                kind: RunnerKind::Ssh,
                server_id: Some("homeboy-lab".to_string()),
                workspace_root: Some("/home/chubes/Developer".to_string()),
                settings: RunnerSettings {
                    homeboy_path: Some("/usr/local/bin/homeboy".to_string()),
                    daemon: true,
                    concurrency_limit: Some(4),
                    artifact_policy: Some("copy".to_string()),
                },
                env: HashMap::from([("RUST_LOG".to_string(), "info".to_string())]),
                resources: HashMap::from([("cpu".to_string(), Value::from(16))]),
            };
            config::save(&legacy).expect("save legacy runner");

            let migrated = migrate_standalone_ssh_runner("lab", false).expect("migrate runner");

            assert_eq!(migrated.id, "homeboy-lab");
            assert_eq!(migrated.server_id.as_deref(), Some("homeboy-lab"));
            assert_eq!(
                migrated.workspace_root.as_deref(),
                Some("/home/chubes/Developer")
            );
            assert_eq!(
                migrated.settings.homeboy_path.as_deref(),
                Some("/usr/local/bin/homeboy")
            );
            assert!(migrated.settings.daemon);
            assert_eq!(migrated.settings.concurrency_limit, Some(4));
            assert_eq!(migrated.settings.artifact_policy.as_deref(), Some("copy"));
            assert_eq!(
                migrated.env.get("RUST_LOG").map(String::as_str),
                Some("info")
            );
            assert_eq!(migrated.resources.get("cpu"), Some(&Value::from(16)));
            assert!(config::exists::<Runner>("lab"));

            let stored_server = server::load("homeboy-lab").expect("load server");
            assert!(stored_server.runner.is_some());
        });
    }

    #[test]
    fn migrate_can_remove_legacy_runner_when_requested() {
        test_support::with_isolated_home(|_| {
            server::create(
                r#"{"id":"homeboy-lab","host":"192.168.86.63","user":"chubes"}"#,
                false,
            )
            .expect("create server");

            let legacy = Runner {
                id: "lab".to_string(),
                kind: RunnerKind::Ssh,
                server_id: Some("homeboy-lab".to_string()),
                workspace_root: Some("/home/chubes/Developer".to_string()),
                settings: RunnerSettings::default(),
                env: HashMap::new(),
                resources: HashMap::new(),
            };
            config::save(&legacy).expect("save legacy runner");

            migrate_standalone_ssh_runner("lab", true).expect("migrate runner");

            assert!(!config::exists::<Runner>("lab"));
            assert!(load("homeboy-lab").is_ok());
        });
    }

    #[test]
    fn default_lab_runner_prefers_configured_connected_runner() {
        let selected = resolve_default_lab_runner_from_candidates(
            Some("lab-b"),
            vec![
                DefaultLabRunnerCandidate {
                    id: "lab-a".to_string(),
                    connected: true,
                },
                DefaultLabRunnerCandidate {
                    id: "lab-b".to_string(),
                    connected: true,
                },
            ],
        );

        assert_eq!(selected.as_deref(), Some("lab-b"));
    }

    #[test]
    fn default_lab_runner_selects_single_runner_when_unconfigured() {
        let selected = resolve_default_lab_runner_from_candidates(
            None,
            vec![
                DefaultLabRunnerCandidate {
                    id: "lab-a".to_string(),
                    connected: false,
                },
                DefaultLabRunnerCandidate {
                    id: "lab-b".to_string(),
                    connected: true,
                },
            ],
        );

        assert_eq!(selected.as_deref(), Some("lab-b"));

        let disconnected = resolve_default_lab_runner_from_candidates(
            None,
            vec![DefaultLabRunnerCandidate {
                id: "lab-a".to_string(),
                connected: false,
            }],
        );

        assert_eq!(disconnected.as_deref(), Some("lab-a"));
    }

    #[test]
    fn default_lab_runner_is_conservative_without_unique_connected_runner() {
        let none_connected_with_multiple_candidates = resolve_default_lab_runner_from_candidates(
            None,
            vec![
                DefaultLabRunnerCandidate {
                    id: "lab-a".to_string(),
                    connected: false,
                },
                DefaultLabRunnerCandidate {
                    id: "lab-b".to_string(),
                    connected: false,
                },
            ],
        );
        let multiple_connected = resolve_default_lab_runner_from_candidates(
            None,
            vec![
                DefaultLabRunnerCandidate {
                    id: "lab-a".to_string(),
                    connected: true,
                },
                DefaultLabRunnerCandidate {
                    id: "lab-b".to_string(),
                    connected: true,
                },
            ],
        );

        assert!(none_connected_with_multiple_candidates.is_none());
        assert!(multiple_connected.is_none());
    }

    #[test]
    fn default_lab_runner_uses_disconnected_preferred_runner() {
        let selected = resolve_default_lab_runner_from_candidates(
            Some("lab-a"),
            vec![DefaultLabRunnerCandidate {
                id: "lab-a".to_string(),
                connected: false,
            }],
        );

        assert_eq!(selected.as_deref(), Some("lab-a"));
    }
}
