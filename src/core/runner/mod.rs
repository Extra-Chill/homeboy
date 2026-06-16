use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::agent_task_secrets;
use crate::core::config::{self, ConfigEntity};
use crate::core::defaults;
use crate::core::error::{Error, Result};
use crate::core::output::{BatchResult, CreateOutput, CreateResult, MergeOutput, MergeResult};
use crate::core::server::{self, RunnerPolicy, RunnerSecretEnvRef, RunnerSettings, ServerRunner};

mod apply;
mod broker_http;
mod capabilities;
mod command_path;
mod connection;
mod daemon_health;
mod evidence;
mod execution;
mod git_dependency_materialization;
mod lab;
mod lab_apply;
mod lab_args;
mod lab_capabilities;
mod lab_command;
mod lab_env;
mod lab_plan;
mod lab_selection;
mod lab_workspaces;
mod offload_changed_since;
mod offload_metadata;
mod origin_refs;
mod resource_metrics;
mod rig_materialization;
mod session;
mod source_materialization;
mod validation_dependencies;
pub(crate) use validation_dependencies::validation_dependency_ids;
pub use validation_dependencies::RunnerValidationDependencySyncOutput;
mod worker;
mod workspace;
pub(crate) use workspace::copy_snapshot_to_directory;

pub use apply::{
    apply_change_artifact, apply_workspace_patch, RunnerWorkspaceApplyOptions,
    RunnerWorkspaceApplyOutput, RunnerWorkspaceApplyStatus,
};
pub use capabilities::{
    evaluate_lab_runner_capabilities_for_runner, prepare_lab_runner_capability,
    LabRunnerCapabilityContract, LabRunnerGateDecision, LabRunnerGateMode,
    PreparedLabRunnerCapability, RunnerCapabilityPreflight, RunnerRequiredTool,
};
pub(crate) use command_path::{
    normalize_runner_command_env, quote_runner_env_value, remote_shell_path_preamble,
};
pub use connection::{connect, connect_reverse, disconnect, status, statuses};
pub(crate) use evidence::artifact_store_locator_from_runner_artifact_id;
pub use evidence::runner_artifact_store_token;
pub use evidence::{
    download_remote_artifact, is_remote_runner_artifact_path, is_reportable_artifact_evidence_path,
    is_retrievable_runner_artifact, mirror_connected_runner_run, mirrored_runner_job_identity,
    refresh_mirrored_daemon_evidence, reportable_artifact_evidence_path, runner_job_log_snapshot,
    RemoteArtifactDownload, RunnerJobLogSnapshot,
};
pub(crate) use execution::{
    daemon_api_get, execute_runner_process_until_cancelled, prepare_daemon_local_process,
    RunnerProcessRequest, RUNNER_HOSTED_EXEC_ENV,
};
pub use execution::{
    daemon_api_post, exec, runner_exec_failure_error, RunnerExecMode, RunnerExecOptions,
    RunnerExecOutput,
};
pub(crate) use git_dependency_materialization::{
    materialize_git_dependency, RunnerGitDependencyMaterializationOptions,
    RunnerGitDependencyMaterializationOutput,
};
pub use lab::{
    execute_lab_offload, LabOffloadCommand, LabOffloadOutcome, LabOffloadRequest,
    LabOffloadSourcePathMode, LabOffloadWorkspaceModePolicy, LabRunnerSelectionSource,
};
pub use offload_changed_since::{
    lab_offload_changed_since_ref, preflight_lab_offload_changed_since,
    prepare_git_lab_offload_changed_since,
};
pub use offload_metadata::{
    capture_lab_offload_subprocess_metadata, lab_offload_metadata,
    lab_offload_metadata_with_workspace_mapping,
};
pub use resource_metrics::RunnerResourceMetrics;
pub use session::{
    ReverseRunnerConnectOptions, RunnerConnectReport, RunnerDisconnectReport, RunnerFailureKind,
    RunnerSession, RunnerSessionRole, RunnerSessionState, RunnerStaleDaemonWarning,
    RunnerStatusReport, RunnerTunnelMode,
};
pub use worker::{run_reverse_worker, ReverseRunnerWorkerOptions, ReverseRunnerWorkerOutput};
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
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub secret_env: HashMap<String, RunnerSecretEnvRef>,
    #[serde(default)]
    pub resources: HashMap<String, Value>,
    #[serde(default, skip_serializing_if = "RunnerPolicy::is_empty")]
    pub policy: RunnerPolicy,
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
    if let Ok(runner) = config::load::<Runner>(id) {
        if runner.kind == RunnerKind::Local {
            return Ok(runner);
        }
    }

    load_server_runner(id)
}

pub fn effective_env(id: &str) -> Result<HashMap<String, String>> {
    let runner = load(id)?;
    let mut env = runner.env.clone();
    normalize_runner_command_env(&mut env);
    Ok(env)
}

pub fn list() -> Result<Vec<Runner>> {
    let mut runners: Vec<Runner> = config::list::<Runner>()?
        .into_iter()
        .filter(|runner| runner.kind == RunnerKind::Local)
        .collect();
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
            let status = status(&runner.id).ok()?;
            let mode = status
                .session
                .as_ref()
                .map_or(RunnerTunnelMode::DirectSsh, |session| session.mode.clone());
            Some(DefaultLabRunnerCandidate {
                id: runner.id,
                mode,
                connected: status.connected,
            })
        }),
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DefaultLabRunnerCandidate {
    id: String,
    mode: RunnerTunnelMode,
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

    if let Ok(runner) = config::load::<Runner>(&effective_id) {
        if runner.kind == RunnerKind::Local {
            return Ok(MergeOutput::Single(config::merge_from_json::<Runner>(
                Some(&effective_id),
                &raw,
                replace_fields,
            )?));
        }
    }

    Ok(MergeOutput::Single(merge_server_runner(
        &effective_id,
        parsed,
        replace_fields,
    )?))
}

pub fn delete_safe(id: &str) -> Result<()> {
    if let Ok(runner) = config::load::<Runner>(id) {
        if runner.kind == RunnerKind::Local {
            return config::delete_safe::<Runner>(id);
        }
    }

    let mut server = server::load(id)?;
    if server.runner.is_none() {
        return Err(Error::runner_not_found(id.to_string(), vec![]));
    }
    server.runner = None;
    server::save(&server)
}

pub fn exists(id: &str) -> bool {
    config::load::<Runner>(id)
        .map(|runner| runner.kind == RunnerKind::Local)
        .unwrap_or(false)
        || load_server_runner(id).is_ok()
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
        secret_env: runner.secret_env,
        resources: runner.resources,
        policy: runner.policy,
    }
}

pub(crate) fn resolve_runner_secret_env(
    secret_env: &HashMap<String, RunnerSecretEnvRef>,
) -> Result<HashMap<String, String>> {
    let mut resolved = HashMap::new();
    for (name, source) in secret_env {
        let has_env = source
            .env
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty());
        let has_file = source
            .file
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty());
        let has_secret = source
            .secret
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty());
        match (has_env, has_file, has_secret) {
            (true, false, false) => {
                let env_name = source.env.as_deref().unwrap_or_default();
                let value = std::env::var(env_name).map_err(|err| {
                    Error::validation_invalid_argument(
                        "secret_env",
                        format!("failed to read secret env ref for {name}: {err}"),
                        Some(env_name.to_string()),
                        Some(vec![
                            "Set the referenced environment variable on the runner process."
                                .to_string(),
                        ]),
                    )
                })?;
                resolved.insert(name.clone(), value);
            }
            (false, true, false) => {
                let raw_path = source.file.as_deref().unwrap_or_default();
                let path = shellexpand::tilde(raw_path).to_string();
                let value = std::fs::read_to_string(&path).map_err(|err| {
                    Error::internal_io(
                        err.to_string(),
                        Some(format!("read secret env file {path}")),
                    )
                })?;
                resolved.insert(
                    name.clone(),
                    value.trim_end_matches(['\r', '\n']).to_string(),
                );
            }
            (false, false, true) => {
                let secret_name = source.secret.as_deref().unwrap_or_default();
                let values = agent_task_secrets::resolve_secret_env(&[secret_name.to_string()])
                    .map_err(|err| {
                        Error::validation_invalid_argument(
                            "secret_env",
                            format!(
                                "failed to resolve Homeboy secret ref for {name}: {}",
                                err.message
                            ),
                            Some(secret_name.to_string()),
                            Some(vec![
                                "Configure the named Homeboy secret before running this runner job."
                                    .to_string(),
                            ]),
                        )
                    })?;
                let value = values
                    .into_iter()
                    .next()
                    .map(|(_, value)| value)
                    .ok_or_else(|| {
                        Error::validation_invalid_argument(
                            "secret_env",
                            format!("Homeboy secret ref for {name} resolved no value"),
                            Some(secret_name.to_string()),
                            None,
                        )
                    })?;
                resolved.insert(name.clone(), value);
            }
            (false, false, false) => {
                return Err(Error::validation_invalid_argument(
                    "secret_env",
                    format!("secret env ref for {name} requires env, file, or secret"),
                    Some(name.clone()),
                    None,
                ));
            }
            _ => {
                return Err(Error::validation_invalid_argument(
                    "secret_env",
                    format!(
                        "secret env ref for {name} must use exactly one of env, file, or secret"
                    ),
                    Some(name.clone()),
                    None,
                ));
            }
        }
    }
    Ok(resolved)
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
    fn runner_registry_persists_trust_policy() {
        test_support::with_isolated_home(|_| {
            let spec = r#"{
                "id": "lab-local",
                "kind": "local",
                "policy": {
                    "accepted_peer_ids": ["extra-chill"],
                    "accepted_peer_fingerprints": ["SHA256:abc123"],
                    "allowed_projects": ["extrachill"],
                    "allowed_commands": ["test", "bench"],
                    "allow_raw_exec": false,
                    "workspace_roots": ["/home/chubes/Developer"],
                    "artifact_policy": "metadata"
                }
            }"#;

            create(spec, false).expect("create runner");
            let runner = load("lab-local").expect("load runner");

            assert_eq!(runner.policy.accepted_peer_ids, vec!["extra-chill"]);
            assert_eq!(
                runner.policy.accepted_peer_fingerprints,
                vec!["SHA256:abc123"]
            );
            assert_eq!(runner.policy.allowed_projects, vec!["extrachill"]);
            assert_eq!(runner.policy.allowed_commands, vec!["test", "bench"]);
            assert_eq!(runner.policy.allow_raw_exec, Some(false));
            assert_eq!(
                runner.policy.workspace_roots,
                vec!["/home/chubes/Developer"]
            );
            assert_eq!(runner.policy.artifact_policy.as_deref(), Some("metadata"));
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
    fn standalone_ssh_runner_config_is_not_loaded_or_listed() {
        test_support::with_isolated_home(|_| {
            server::create(
                r#"{"id":"homeboy-lab","host":"192.168.86.63","user":"chubes"}"#,
                false,
            )
            .expect("create server");

            let standalone_ssh_runner = Runner {
                id: "lab".to_string(),
                kind: RunnerKind::Ssh,
                server_id: Some("homeboy-lab".to_string()),
                workspace_root: Some("/home/chubes/Developer".to_string()),
                settings: RunnerSettings::default(),
                env: HashMap::new(),
                secret_env: HashMap::new(),
                resources: HashMap::new(),
                policy: RunnerPolicy::default(),
            };
            config::save(&standalone_ssh_runner).expect("save standalone ssh runner");

            assert_eq!(
                load("lab")
                    .expect_err("standalone ssh ignored")
                    .code
                    .as_str(),
                "server.not_found"
            );
            assert!(!exists("lab"));
            assert!(list().expect("list runners").is_empty());
        });
    }

    #[test]
    fn runner_secret_env_refs_resolve_from_env_and_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let secret_file = temp.path().join("runner-token");
        std::fs::write(&secret_file, "dummy-file-secret\n").expect("write dummy secret");
        std::env::set_var("HOMEBOY_DUMMY_SECRET_REF", "dummy-env-secret");

        let resolved = resolve_runner_secret_env(&HashMap::from([
            (
                "FROM_ENV".to_string(),
                server::RunnerSecretEnvRef {
                    env: Some("HOMEBOY_DUMMY_SECRET_REF".to_string()),
                    file: None,
                    secret: None,
                },
            ),
            (
                "FROM_FILE".to_string(),
                server::RunnerSecretEnvRef {
                    env: None,
                    file: Some(secret_file.display().to_string()),
                    secret: None,
                },
            ),
        ]))
        .expect("resolve secret refs");

        assert_eq!(
            resolved.get("FROM_ENV").map(String::as_str),
            Some("dummy-env-secret")
        );
        assert_eq!(
            resolved.get("FROM_FILE").map(String::as_str),
            Some("dummy-file-secret")
        );
        std::env::remove_var("HOMEBOY_DUMMY_SECRET_REF");
    }

    #[test]
    fn runner_secret_env_refs_resolve_from_configured_homeboy_secret() {
        crate::test_support::with_isolated_home(|_| {
            crate::core::agent_task_secrets::set_config_secret(
                "HOMEBOY_DUMMY_CONFIGURED_SECRET",
                "dummy-configured-secret",
            )
            .expect("configure secret");

            let resolved = resolve_runner_secret_env(&HashMap::from([(
                "FROM_SECRET".to_string(),
                server::RunnerSecretEnvRef {
                    env: None,
                    file: None,
                    secret: Some("HOMEBOY_DUMMY_CONFIGURED_SECRET".to_string()),
                },
            )]))
            .expect("resolve configured secret ref");

            assert_eq!(
                resolved.get("FROM_SECRET").map(String::as_str),
                Some("dummy-configured-secret")
            );
        });
    }

    #[test]
    fn runner_secret_env_refs_reject_multiple_sources() {
        let err = resolve_runner_secret_env(&HashMap::from([(
            "INVALID".to_string(),
            server::RunnerSecretEnvRef {
                env: Some("HOMEBOY_DUMMY_SECRET_REF".to_string()),
                file: None,
                secret: Some("HOMEBOY_DUMMY_CONFIGURED_SECRET".to_string()),
            },
        )]))
        .expect_err("multiple sources rejected");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("exactly one"));
    }

    #[test]
    fn default_lab_runner_prefers_configured_connected_runner() {
        let selected = resolve_default_lab_runner_from_candidates(
            Some("lab-b"),
            vec![
                DefaultLabRunnerCandidate {
                    id: "lab-a".to_string(),
                    mode: RunnerTunnelMode::DirectSsh,
                    connected: true,
                },
                DefaultLabRunnerCandidate {
                    id: "lab-b".to_string(),
                    mode: RunnerTunnelMode::Reverse,
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
                    mode: RunnerTunnelMode::DirectSsh,
                    connected: false,
                },
                DefaultLabRunnerCandidate {
                    id: "lab-b".to_string(),
                    mode: RunnerTunnelMode::Reverse,
                    connected: true,
                },
            ],
        );

        assert_eq!(selected.as_deref(), Some("lab-b"));

        let disconnected = resolve_default_lab_runner_from_candidates(
            None,
            vec![DefaultLabRunnerCandidate {
                id: "lab-a".to_string(),
                mode: RunnerTunnelMode::DirectSsh,
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
                    mode: RunnerTunnelMode::DirectSsh,
                    connected: false,
                },
                DefaultLabRunnerCandidate {
                    id: "lab-b".to_string(),
                    mode: RunnerTunnelMode::Reverse,
                    connected: false,
                },
            ],
        );
        let multiple_connected = resolve_default_lab_runner_from_candidates(
            None,
            vec![
                DefaultLabRunnerCandidate {
                    id: "lab-a".to_string(),
                    mode: RunnerTunnelMode::DirectSsh,
                    connected: true,
                },
                DefaultLabRunnerCandidate {
                    id: "lab-b".to_string(),
                    mode: RunnerTunnelMode::Reverse,
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
                mode: RunnerTunnelMode::Reverse,
                connected: false,
            }],
        );

        assert_eq!(selected.as_deref(), Some("lab-a"));
    }

    #[test]
    fn default_lab_runner_can_select_connected_reverse_runner() {
        let selected = resolve_default_lab_runner_from_candidates(
            None,
            vec![DefaultLabRunnerCandidate {
                id: "homeboy-lab".to_string(),
                mode: RunnerTunnelMode::Reverse,
                connected: true,
            }],
        );

        assert_eq!(selected.as_deref(), Some("homeboy-lab"));
    }
}
