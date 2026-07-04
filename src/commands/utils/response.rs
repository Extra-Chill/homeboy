//! CLI response formatting and output.
//!
//! Provides JSON envelope, printing, and exit code mapping.

use homeboy::core::error::Hint;
use homeboy::core::{Error, ErrorCode, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::output::{write_output_file_atomically, OutputWriteOptions};

const COMMAND_RESULT_SCHEMA: &str = "homeboy/command-result/v3";
pub const ACTIONABLE_METADATA_KEY: &str = "_homeboy_actionable";

#[derive(Debug, Serialize)]
pub struct CommandResultEnvelope<T: Serialize> {
    pub schema: &'static str,
    pub command: String,
    pub success: bool,
    pub exit_code: i32,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<CommandRunRef>,
    #[serde(default, skip_serializing_if = "CommandResultRefs::is_empty")]
    pub refs: CommandResultRefs,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_actions: Vec<CommandNextAction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<CommandArtifactRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<CommandEvidenceRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<CommandDiagnostics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presentation: Option<CommandPresentationEnvelope>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CommandActionableMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<CommandRunRef>,
    #[serde(default, skip_serializing_if = "CommandResultRefs::is_empty")]
    pub refs: CommandResultRefs,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_actions: Vec<CommandNextAction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<CommandArtifactRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<CommandEvidenceRef>,
}

impl CommandActionableMetadata {
    pub fn is_empty(&self) -> bool {
        self.run.is_none()
            && self.refs.is_empty()
            && self.next_actions.is_empty()
            && self.artifacts.is_empty()
            && self.evidence.is_empty()
    }

    pub fn for_run(run: CommandRunRef) -> Self {
        Self {
            run: Some(run.clone()),
            refs: CommandResultRefs {
                runs: vec![run],
                ..Default::default()
            },
            ..Default::default()
        }
    }

    pub fn with_next_action(mut self, action: CommandNextAction) -> Self {
        self.next_actions.push(action);
        self
    }

    pub fn with_artifact(mut self, artifact: CommandArtifactRef) -> Self {
        self.artifacts.push(artifact);
        self
    }
}

pub fn actionable_metadata_for_run_ref(
    run_id: impl Into<String>,
    kind: impl Into<String>,
    source: impl Into<String>,
) -> CommandActionableMetadata {
    let run_id = run_id.into();
    CommandActionableMetadata::for_run(CommandRunRef {
        id: run_id.clone(),
        kind: kind.into(),
        source: source.into(),
        location: None,
        started_at: None,
        updated_at: None,
        finished_at: None,
        status_command: format!("homeboy runs show {run_id}"),
        watch_command: format!("homeboy runs watch {run_id}"),
    })
    .with_next_action(
        CommandNextAction::new("show evidence", format!("homeboy runs evidence {run_id}"))
            .with_kind(CommandNextActionKind::Show),
    )
    .with_next_action(
        CommandNextAction::new("show activity", format!("homeboy activity show {run_id}"))
            .with_kind(CommandNextActionKind::Show),
    )
}

pub fn actionable_metadata_value_for_run_ref(
    run_id: impl Into<String>,
    kind: impl Into<String>,
    source: impl Into<String>,
) -> serde_json::Value {
    serde_json::to_value(actionable_metadata_for_run_ref(run_id, kind, source))
        .unwrap_or(serde_json::Value::Null)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CommandResultRefs {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<CommandRunRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jobs: Vec<CommandJobRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agent_tasks: Vec<CommandAgentTaskRef>,
}

impl CommandResultRefs {
    pub fn is_empty(&self) -> bool {
        self.runs.is_empty() && self.jobs.is_empty() && self.agent_tasks.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandNextAction {
    pub label: String,
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<CommandNextActionKind>,
}

impl CommandNextAction {
    pub fn new(label: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            command: command.into(),
            kind: None,
        }
    }

    pub fn with_kind(mut self, kind: CommandNextActionKind) -> Self {
        self.kind = Some(kind);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandNextActionKind {
    Watch,
    Show,
    Artifacts,
    Repair,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandRunRef {
    pub id: String,
    pub kind: String,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    pub status_command: String,
    pub watch_command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandJobRef {
    pub id: String,
    pub kind: String,
    pub source: String,
    pub status_command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub watch_command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandAgentTaskRef {
    pub id: String,
    pub source: String,
    pub status_command: String,
    pub logs_command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandArtifactRef {
    pub id: String,
    pub kind: String,
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandEvidenceRef {
    pub id: String,
    pub kind: String,
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_key: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CommandDiagnostics {
    pub code: String,
    pub message: String,
    pub details: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hints: Option<Vec<Hint>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_digest: Option<CommandFailureDigest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandFailureDigest {
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout_tail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr_tail: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<CommandArtifactRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_actions: Vec<CommandNextAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CommandPresentationEnvelope {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
}

impl<T: Serialize> CommandResultEnvelope<T> {
    pub fn success(command: &str, data: T) -> Self {
        Self {
            schema: COMMAND_RESULT_SCHEMA,
            command: command.to_string(),
            success: true,
            exit_code: 0,
            status: "succeeded".to_string(),
            run: None,
            refs: CommandResultRefs::default(),
            summary: None,
            next_actions: Vec::new(),
            artifacts: Vec::new(),
            evidence: Vec::new(),
            diagnostics: None,
            data: Some(data),
            presentation: None,
        }
    }

    fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self).map_err(|e| {
            Error::internal_json(e.to_string(), Some("serialize response".to_string()))
        })
    }
}

impl CommandResultEnvelope<()> {
    fn from_error(command: &str, err: &Error, exit_code: i32) -> Self {
        Self {
            schema: COMMAND_RESULT_SCHEMA,
            command: command.to_string(),
            success: false,
            exit_code,
            status: status_for_result(None, exit_code),
            run: None,
            refs: CommandResultRefs::default(),
            summary: Some(err.message.clone()),
            next_actions: Vec::new(),
            artifacts: Vec::new(),
            evidence: Vec::new(),
            diagnostics: Some(CommandDiagnostics {
                code: err.code.as_str().to_string(),
                message: err.message.clone(),
                details: err.details.clone(),
                hints: if err.hints.is_empty() {
                    None
                } else {
                    Some(err.hints.clone())
                },
                retryable: err.retryable,
                failure_digest: failure_digest_for_error(err),
            }),
            data: None,
            presentation: None,
        }
    }
}

fn print_response<T: Serialize>(response: &CommandResultEnvelope<T>) -> Result<()> {
    use std::io::{self, Write};

    let payload = response.to_json()?;
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    if let Err(e) = writeln!(handle, "{}", payload) {
        if e.kind() == io::ErrorKind::BrokenPipe {
            return Ok(()); // Exit gracefully on SIGPIPE
        }
        return Err(Error::internal_io(
            e.to_string(),
            Some("write stdout".to_string()),
        ));
    }
    Ok(())
}

pub fn print_success<T: Serialize>(data: T) -> Result<()> {
    print_response(&CommandResultEnvelope::success("unknown", data))
}

pub fn print_result<T: Serialize>(result: Result<T>) -> Result<()> {
    match result {
        Ok(data) => print_success(data),
        Err(err) => print_response(&CommandResultEnvelope::<()>::from_error(
            "unknown",
            &err,
            exit_code_for_error(err.code),
        )),
    }
}

pub fn map_cmd_result_to_json<T: Serialize>(
    result: Result<(T, i32)>,
) -> (Result<serde_json::Value>, i32) {
    match result {
        Ok((data, exit_code)) => match serde_json::to_value(data) {
            Ok(value) => (Ok(value), exit_code),
            Err(err) => (
                Err(Error::internal_json(
                    err.to_string(),
                    Some("serialize response".to_string()),
                )),
                1,
            ),
        },
        Err(err) => {
            let exit_code = exit_code_for_error(err.code);
            (Err(err), exit_code)
        }
    }
}

fn exit_code_for_error(code: ErrorCode) -> i32 {
    match code {
        ErrorCode::ConfigMissingKey
        | ErrorCode::ConfigInvalidJson
        | ErrorCode::ConfigInvalidValue
        | ErrorCode::ConfigIdCollision
        | ErrorCode::ValidationMissingArgument
        | ErrorCode::ValidationInvalidArgument
        | ErrorCode::ValidationInvalidJson
        | ErrorCode::RigSchemaUnsupported
        | ErrorCode::ValidationMultipleErrors => 2,

        ErrorCode::ProjectNotFound
        | ErrorCode::ServerNotFound
        | ErrorCode::ComponentNotFound
        | ErrorCode::ComponentNotAttached
        | ErrorCode::FleetNotFound
        | ErrorCode::ExtensionNotFound
        | ErrorCode::ExtensionUnsupported
        | ErrorCode::DocsTopicNotFound
        | ErrorCode::RigNotFound
        | ErrorCode::RunnerNotFound
        | ErrorCode::ServiceTunnelNotFound
        | ErrorCode::StackNotFound
        | ErrorCode::ProjectNoActive => 4,

        ErrorCode::RigPipelineFailed
        | ErrorCode::RunnerPolicyDenied
        | ErrorCode::RunnerCapabilityMissing
        | ErrorCode::BrokerAuthDenied
        | ErrorCode::RigServiceFailed
        | ErrorCode::RigResourceConflict
        | ErrorCode::RunnerLabTransportFailure
        | ErrorCode::RunnerControllerDisconnected
        | ErrorCode::StackApplyConflict
        | ErrorCode::DependencyStepFailed
        | ErrorCode::DependencyOutputMissing => 20,

        ErrorCode::SshServerInvalid
        | ErrorCode::SshIdentityFileNotFound
        | ErrorCode::SshAuthFailed
        | ErrorCode::SshConnectFailed => 10,

        ErrorCode::RemoteCommandFailed
        | ErrorCode::RemoteCommandTimeout
        | ErrorCode::DeployNoComponentsConfigured
        | ErrorCode::DeployBuildFailed
        | ErrorCode::DeployUploadFailed
        | ErrorCode::GitCommandFailed => 20,

        ErrorCode::InternalIoError
        | ErrorCode::InternalJsonError
        | ErrorCode::InternalUnexpected => 1,
    }
}

pub fn print_json_result(result: Result<serde_json::Value>, exit_code: i32) -> Result<()> {
    print_json_result_for_command(result, exit_code, "unknown", None)
}

pub fn print_json_result_for_command(
    result: Result<Value>,
    exit_code: i32,
    command: &str,
    presentation: Option<CommandPresentationEnvelope>,
) -> Result<()> {
    print_response(&cli_response_for_json_result_for_command(
        &result,
        exit_code,
        command,
        presentation,
    ))
}

pub fn cli_response_for_json_result(
    result: &Result<serde_json::Value>,
    exit_code: i32,
) -> CommandResultEnvelope<serde_json::Value> {
    cli_response_for_json_result_for_command(result, exit_code, "unknown", None)
}

pub fn cli_response_for_json_result_for_command(
    result: &Result<serde_json::Value>,
    exit_code: i32,
    command: &str,
    presentation: Option<CommandPresentationEnvelope>,
) -> CommandResultEnvelope<serde_json::Value> {
    match result {
        Ok(data) => envelope_for_data(command, data.clone(), exit_code, presentation),
        Err(err) => CommandResultEnvelope::<()>::from_error(command, err, exit_code).into_value(),
    }
}

impl CommandResultEnvelope<()> {
    fn into_value(self) -> CommandResultEnvelope<Value> {
        CommandResultEnvelope {
            schema: self.schema,
            command: self.command,
            success: self.success,
            exit_code: self.exit_code,
            status: self.status,
            run: self.run,
            refs: self.refs,
            summary: self.summary,
            next_actions: self.next_actions,
            artifacts: self.artifacts,
            evidence: self.evidence,
            diagnostics: self.diagnostics,
            data: None,
            presentation: self.presentation,
        }
    }
}

fn envelope_for_data(
    command: &str,
    mut data: Value,
    exit_code: i32,
    presentation: Option<CommandPresentationEnvelope>,
) -> CommandResultEnvelope<Value> {
    let success = exit_code == 0;
    let mut actionable = actionable_metadata_for_payload(&mut data).unwrap_or_default();
    if actionable.run.is_none() {
        actionable.run = actionable.refs.runs.first().cloned();
    }
    let run = actionable.run;
    let refs = actionable.refs;
    let artifacts = actionable.artifacts;
    let mut evidence = actionable.evidence;

    if evidence.is_empty() {
        if let Some(run) = &run {
            evidence.push(CommandEvidenceRef {
                id: format!("{}-result", run.id),
                kind: "command-result".to_string(),
                uri: format!("homeboy://runs/{}/result", run.id),
                semantic_key: Some("command_result".to_string()),
            });
        }
    }

    let diagnostics = failure_diagnostics_for_data(exit_code, &run, &artifacts);

    CommandResultEnvelope {
        schema: COMMAND_RESULT_SCHEMA,
        command: command.to_string(),
        success,
        exit_code,
        status: status_for_result(Some(&data), exit_code),
        run,
        refs,
        summary: summary_for_payload(&data, presentation.as_ref()),
        next_actions: actionable.next_actions,
        artifacts,
        evidence,
        diagnostics,
        data: Some(data),
        presentation,
    }
}

fn failure_diagnostics_for_data(
    exit_code: i32,
    run: &Option<CommandRunRef>,
    artifacts: &[CommandArtifactRef],
) -> Option<CommandDiagnostics> {
    if exit_code == 0 {
        return None;
    }
    let failure_digest = run
        .as_ref()
        .and_then(|run| failure_digest_for_run(&run.id, artifacts));
    failure_digest.map(|failure_digest| CommandDiagnostics {
        code: "command.failed".to_string(),
        message: failure_digest.summary.clone(),
        details: serde_json::json!({ "exit_code": exit_code }),
        hints: None,
        retryable: failure_digest.retryable,
        failure_digest: Some(failure_digest),
    })
}

fn failure_digest_for_error(err: &Error) -> Option<CommandFailureDigest> {
    match err.code {
        ErrorCode::RemoteCommandFailed => Some(CommandFailureDigest {
            summary: remote_failure_summary(&err.details),
            stdout_tail: string_at(&err.details, &["stdout"]).map(tail_text),
            stderr_tail: string_at(&err.details, &["stderr"]).map(tail_text),
            artifact_refs: Vec::new(),
            next_actions: Vec::new(),
            retryable: err.retryable,
        }),
        _ => None,
    }
}

fn failure_digest_for_run(
    run_id: &str,
    artifacts: &[CommandArtifactRef],
) -> Option<CommandFailureDigest> {
    let store = homeboy::core::observation::ObservationStore::open_initialized().ok()?;
    let run = store.get_run(run_id).ok().flatten()?;
    let failure = homeboy::core::observation::evidence_report::evidence_failure_summary(&run);
    if !failure.failed {
        return None;
    }
    let mut summary = failure
        .error
        .clone()
        .or_else(|| failure.gate_failures.first().cloned())
        .unwrap_or_else(|| format!("{} run {} failed", run.kind, run.id));
    if let Some(exit_code) = failure.exit_code {
        summary = format!("{summary} (exit {exit_code})");
    }
    let artifact_refs = if artifacts.is_empty() {
        store
            .list_artifacts(run_id)
            .ok()
            .unwrap_or_default()
            .into_iter()
            .take(10)
            .map(|artifact| CommandArtifactRef {
                id: artifact.id.clone(),
                kind: artifact.artifact_type,
                uri: artifact.path,
                semantic_key: Some(artifact.kind),
            })
            .collect()
    } else {
        artifacts.to_vec()
    };
    Some(CommandFailureDigest {
        summary,
        stdout_tail: None,
        stderr_tail: None,
        artifact_refs,
        next_actions: vec![
            CommandNextAction::new("show evidence", format!("homeboy runs evidence {run_id}"))
                .with_kind(CommandNextActionKind::Show),
            CommandNextAction::new("show activity", format!("homeboy activity show {run_id}"))
                .with_kind(CommandNextActionKind::Show),
        ],
        retryable: None,
    })
}

fn remote_failure_summary(details: &Value) -> String {
    let command = string_at(details, &["command"]).unwrap_or_else(|| "remote command".to_string());
    let exit_code = details.get("exit_code").and_then(Value::as_i64);
    match exit_code {
        Some(code) => format!("{command} failed with exit code {code}"),
        None => format!("{command} failed"),
    }
}

fn tail_text(value: String) -> String {
    const MAX_CHARS: usize = 4000;
    let chars = value.chars().count();
    if chars <= MAX_CHARS {
        return value;
    }
    value.chars().skip(chars - MAX_CHARS).collect()
}

fn status_for_result(data: Option<&Value>, exit_code: i32) -> String {
    if exit_code != 0 {
        return "failed".to_string();
    }

    data.and_then(|value| value.get("status").and_then(Value::as_str))
        .and_then(normalize_status)
        .unwrap_or("succeeded")
        .to_string()
}

fn normalize_status(status: &str) -> Option<&'static str> {
    match status.to_ascii_lowercase().as_str() {
        "queued" => Some("queued"),
        "running" | "in_progress" | "active" => Some("running"),
        "succeeded" | "success" | "passed" | "pass" | "complete" | "completed" => Some("succeeded"),
        "partial_failure" | "partial-failure" | "partial" => Some("partial_failure"),
        "failed" | "failure" | "error" => Some("failed"),
        "cancelled" | "canceled" => Some("cancelled"),
        "timed_out" | "timed-out" | "timeout" => Some("timed_out"),
        "stale" => Some("stale"),
        _ => None,
    }
}

fn actionable_metadata_for_payload(data: &mut Value) -> Option<CommandActionableMetadata> {
    match data {
        Value::Object(map) => {
            if let Some(metadata) = map.remove(ACTIONABLE_METADATA_KEY) {
                return serde_json::from_value(metadata).ok();
            }
            for child in map.values_mut() {
                if let Some(metadata) = actionable_metadata_for_payload(child) {
                    return Some(metadata);
                }
            }
            None
        }
        Value::Array(items) => {
            for child in items {
                if let Some(metadata) = actionable_metadata_for_payload(child) {
                    return Some(metadata);
                }
            }
            None
        }
        _ => None,
    }
}

fn summary_for_payload(
    data: &Value,
    presentation: Option<&CommandPresentationEnvelope>,
) -> Option<String> {
    presentation
        .and_then(|presentation| presentation.stdout.clone())
        .or_else(|| string_at(data, &["summary"]))
        .or_else(|| string_at(data, &["message"]))
        .map(|summary| summary.chars().take(4000).collect())
}

fn string_at(value: &Value, path: &[&str]) -> Option<String> {
    let mut current = value;
    for segment in path {
        current = current.get(*segment)?;
    }
    current.as_str().map(str::to_string)
}

/// Write the JSON output envelope to a file. Best-effort — failures are
/// logged to stderr but don't affect the command's exit code.
pub fn write_json_to_file(result: &Result<serde_json::Value>, path: &str, exit_code: i32) {
    write_json_to_file_for_command(result, path, exit_code, "unknown", None);
}

pub fn write_json_to_file_for_command(
    result: &Result<serde_json::Value>,
    path: &str,
    exit_code: i32,
    command: &str,
    presentation: Option<CommandPresentationEnvelope>,
) {
    let response =
        cli_response_for_json_result_for_command(result, exit_code, command, presentation);

    let json = match serde_json::to_string_pretty(&response) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("Warning: failed to serialize JSON for --output: {}", e);
            return;
        }
    };

    if let Err(e) = write_output_file_atomically(path, json, OutputWriteOptions::json_output()) {
        eprintln!("Warning: failed to write --output file '{}': {}", path, e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_mapping_preserves_success_payload_and_exit_code() {
        let (payload, exit_code) = map_cmd_result_to_json(Ok((json!({ "changed": 2 }), 20)));

        assert_eq!(exit_code, 20);
        assert_eq!(payload.expect("payload"), json!({ "changed": 2 }));
    }

    #[test]
    fn json_mapping_turns_validation_errors_into_cli_exit_code() {
        let err = Error::validation_missing_argument(vec!["component".to_string()]);
        let (payload, exit_code) = map_cmd_result_to_json::<serde_json::Value>(Err(err));

        assert_eq!(exit_code, 2);
        assert_eq!(
            payload.expect_err("error payload").code,
            ErrorCode::ValidationMissingArgument
        );
    }

    #[test]
    fn output_file_write_is_atomic_and_final_json_only() {
        let dir = tempfile::tempdir().expect("temp dir");
        let output_path = dir.path().join("run-plan-output.json");
        std::fs::write(&output_path, r#"{"success":false,"data":{"old":true}}"#)
            .expect("write existing output");

        write_json_to_file(
            &Ok(json!({ "run_id": "run-plan-atomic", "complete": true })),
            output_path.to_str().expect("utf8 path"),
            0,
        );

        let raw = std::fs::read_to_string(&output_path).expect("read output");
        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("final output json");
        assert_eq!(parsed["schema"], COMMAND_RESULT_SCHEMA);
        assert_eq!(parsed["success"], true);
        assert_eq!(parsed["data"]["run_id"], "run-plan-atomic");
        assert!(parsed.get("run").is_none());
        assert_eq!(parsed["data"]["complete"], true);
        assert!(
            std::fs::read_dir(dir.path())
                .expect("read dir")
                .all(|entry| !entry
                    .expect("dir entry")
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")),
            "temporary output file should not remain after successful rename"
        );
    }

    #[test]
    fn json_envelope_uses_v3_contract_and_embeds_typed_actionable_metadata() {
        let response = cli_response_for_json_result_for_command(
            &Ok(json!({
                "run_id": "run-123",
                "hints": ["not lifted"],
                ACTIONABLE_METADATA_KEY: {
                    "run": {
                        "id": "run-123",
                        "kind": "bench",
                        "source": "test",
                        "location": null,
                        "started_at": null,
                        "updated_at": null,
                        "finished_at": null,
                        "status_command": "homeboy runs show run-123",
                        "watch_command": "homeboy runs watch run-123"
                    },
                    "refs": {
                        "runs": [{
                            "id": "run-123",
                            "kind": "bench",
                            "source": "test",
                            "location": null,
                            "started_at": null,
                            "updated_at": null,
                            "finished_at": null,
                            "status_command": "homeboy runs show run-123",
                            "watch_command": "homeboy runs watch run-123"
                        }]
                    },
                    "next_actions": [{
                        "label": "show run",
                        "command": "homeboy runs show run-123",
                        "kind": "show"
                    }],
                    "artifacts": [{
                        "id": "artifact-1",
                        "kind": "file",
                        "uri": "/tmp/artifact.json",
                        "semantic_key": "report"
                    }],
                    "evidence": [{
                        "id": "evidence-1",
                        "kind": "command-result",
                        "uri": "homeboy://runs/run-123/result",
                        "semantic_key": "command_result"
                    }]
                }
            })),
            0,
            "observe",
            Some(CommandPresentationEnvelope {
                stdout: Some("Observed 3 events\n".to_string()),
                stderr: None,
            }),
        );
        let value = serde_json::to_value(response).expect("response json");

        assert_eq!(value["schema"], COMMAND_RESULT_SCHEMA);
        assert_eq!(value["command"], "observe");
        assert_eq!(value["status"], "succeeded");
        assert_eq!(value["run"]["id"], "run-123");
        assert_eq!(value["refs"]["runs"][0]["id"], "run-123");
        assert_eq!(value["run"]["status_command"], "homeboy runs show run-123");
        assert_eq!(value["presentation"]["stdout"], "Observed 3 events\n");
        assert_eq!(value["summary"], "Observed 3 events\n");
        assert_eq!(value["next_actions"][0]["label"], "show run");
        assert_eq!(
            value["next_actions"][0]["command"],
            "homeboy runs show run-123"
        );
        assert_eq!(value["artifacts"][0]["uri"], "/tmp/artifact.json");
        assert!(value["data"].get(ACTIONABLE_METADATA_KEY).is_none());
    }

    #[test]
    fn unmigrated_payloads_do_not_get_heuristic_actionable_fields() {
        let response = cli_response_for_json_result_for_command(
            &Ok(json!({
                "run_id": "run-123",
                "hints": ["homeboy runs show run-123"],
                "artifact_path": "/tmp/artifact.json",
                "evidence": ["homeboy://runs/run-123/result"]
            })),
            0,
            "observe",
            None,
        );
        let value = serde_json::to_value(response).expect("response json");

        assert!(value.get("run").is_none());
        assert!(value.get("refs").is_none());
        assert!(value.get("next_actions").is_none());
        assert!(value.get("artifacts").is_none());
        assert!(value.get("evidence").is_none());
    }

    #[test]
    fn remote_command_failures_include_typed_failure_digest() {
        let err = Error::remote_command_failed(homeboy::core::error::RemoteCommandFailedDetails {
            command: "ssh host false".to_string(),
            exit_code: 23,
            stdout: "before\nstdout tail".to_string(),
            stderr: "before\nstderr tail".to_string(),
            target: homeboy::core::error::TargetDetails {
                project_id: None,
                server_id: Some("prod".to_string()),
                host: Some("example.test".to_string()),
            },
        });
        let response = cli_response_for_json_result_for_command(&Err(err), 20, "deploy", None);
        let value = serde_json::to_value(response).expect("response json");

        assert_eq!(
            value["diagnostics"]["failure_digest"]["summary"],
            "ssh host false failed with exit code 23"
        );
        assert_eq!(
            value["diagnostics"]["failure_digest"]["stdout_tail"],
            "before\nstdout tail"
        );
        assert_eq!(
            value["diagnostics"]["failure_digest"]["stderr_tail"],
            "before\nstderr tail"
        );
    }

    #[test]
    fn failed_run_payload_includes_evidence_failure_digest() {
        crate::test_support::with_isolated_home(|_home| {
            let store =
                homeboy::core::observation::ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(
                    homeboy::core::observation::NewRunRecord::builder("test")
                        .component_id("homeboy")
                        .command("homeboy review test homeboy")
                        .cwd_path(std::path::Path::new("/tmp/homeboy-fixture"))
                        .metadata(json!({ "exit_code": 1, "error": "fixture failure" }))
                        .build(),
                )
                .expect("start run");
            store
                .finish_run(
                    &run.id,
                    homeboy::core::observation::RunStatus::Fail,
                    Some(json!({ "exit_code": 1, "error": "fixture failure" })),
                )
                .expect("finish run");

            let response = cli_response_for_json_result_for_command(
                &Ok(json!({
                    ACTIONABLE_METADATA_KEY: actionable_metadata_value_for_run_ref(
                        run.id.clone(),
                        "test",
                        "test-fixture",
                    )
                })),
                1,
                "test",
                None,
            );
            let value = serde_json::to_value(response).expect("response json");

            assert_eq!(
                value["diagnostics"]["failure_digest"]["summary"],
                "fixture failure (exit 1)"
            );
            assert_eq!(
                value["diagnostics"]["failure_digest"]["next_actions"][0]["command"],
                format!("homeboy runs evidence {}", run.id)
            );
            assert_eq!(value["run"]["id"], run.id);
        });
    }
}
