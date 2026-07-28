//! CLI response formatting and output.
//!
//! Provides JSON envelope, printing, and exit code mapping.

use homeboy::core::error::{ExecutableAction, Hint, ACTIONS_DETAILS_KEY};
use homeboy::core::{Error, ErrorCode, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::core::io::output_file::{write_output_file_atomically, OutputWriteOptions};

const COMMAND_RESULT_SCHEMA: &str = "homeboy/command-result/v3";
pub const ACTIONABLE_METADATA_KEY: &str = "_homeboy_actionable";

#[derive(Debug, Serialize)]
pub struct CommandResultEnvelope<T: Serialize> {
    pub schema: &'static str,
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
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
    pub evidence: Vec<CommandArtifactRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<CommandDiagnostics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presentation: Option<CommandPresentationEnvelope>,
}

/// The resolved CLI identity carried by command-result envelopes. `command` is
/// always the top-level command; nested subcommands are represented by the
/// optional operation without changing the v3 envelope's existing fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandIdentity {
    pub command: String,
    pub operation: Option<String>,
}

impl CommandIdentity {
    pub fn top_level(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            operation: None,
        }
    }

    pub fn with_operation(command: impl Into<String>, operation: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            operation: Some(operation.into()),
        }
    }
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
    pub evidence: Vec<CommandArtifactRef>,
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
    /// The executable form of a repair action. `command` remains the stable
    /// human-facing rendering for existing consumers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<ExecutableAction>,
}

impl CommandNextAction {
    pub fn new(label: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            command: command.into(),
            kind: None,
            action: None,
        }
    }

    pub fn with_kind(mut self, kind: CommandNextActionKind) -> Self {
        self.kind = Some(kind);
        self
    }

    pub fn from_action(action: ExecutableAction) -> Self {
        Self {
            label: action.label.clone(),
            command: action.render_command(),
            kind: Some(CommandNextActionKind::Repair),
            action: Some(action),
        }
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

/// A pointer at one artifact or piece of evidence carried by a command-result
/// envelope.
///
/// `artifacts` and `evidence` on [`CommandActionableMetadata`] both hold this
/// shape. They used to hold two structurally identical types
/// (`CommandArtifactRef` / `CommandEvidenceRef`); the distinction was never in
/// the type, only in which field the value landed in — which is exactly how
/// core already models it (`ActivityItem.artifacts` and `ActivityItem.evidence`
/// are both `Vec<ActivityEvidenceRef>`). Collapsed in #10310; the serialized
/// shape is unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandArtifactRef {
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
            operation: None,
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
    fn from_error(identity: &CommandIdentity, err: &Error, exit_code: i32) -> Self {
        let next_actions = actions_for_error(err);
        Self {
            schema: COMMAND_RESULT_SCHEMA,
            command: identity.command.clone(),
            operation: identity.operation.clone(),
            success: false,
            exit_code,
            status: status_for_result(None, exit_code),
            run: None,
            refs: CommandResultRefs::default(),
            summary: Some(err.message.clone()),
            next_actions,
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
            &CommandIdentity::top_level("unknown"),
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
        | ErrorCode::ScheduleNotFound
        | ErrorCode::ServiceTunnelNotFound
        | ErrorCode::StackNotFound
        | ErrorCode::ProjectNoActive => 4,

        ErrorCode::RigPipelineFailed
        | ErrorCode::RunnerPolicyDenied
        | ErrorCode::RunnerCapabilityMissing
        | ErrorCode::RunnerWorkspaceOwnershipConflict
        | ErrorCode::BrokerAuthDenied
        | ErrorCode::RigServiceFailed
        | ErrorCode::RigResourceConflict
        | ErrorCode::RunnerLabTransportFailure
        | ErrorCode::RunnerControllerDisconnected
        | ErrorCode::RuntimePromotionContended
        | ErrorCode::RuntimePromotionWaitTimeout
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

        ErrorCode::ObservationStoreBusy => 20,

        // A contended runtime promotion (another owner holds the lease) is a
        // transient "busy" condition, not a hard failure — map it to the
        // general error code alongside the other internal/unexpected states.
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
    print_json_result_for_identity(
        result,
        exit_code,
        &CommandIdentity::top_level(command),
        presentation,
    )
}

pub fn print_json_result_for_identity(
    result: Result<Value>,
    exit_code: i32,
    identity: &CommandIdentity,
    presentation: Option<CommandPresentationEnvelope>,
) -> Result<()> {
    print_response(&cli_response_for_json_result_for_identity(
        &result,
        exit_code,
        identity,
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
    cli_response_for_json_result_for_identity(
        result,
        exit_code,
        &CommandIdentity::top_level(command),
        presentation,
    )
}

pub fn cli_response_for_json_result_for_identity(
    result: &Result<serde_json::Value>,
    exit_code: i32,
    identity: &CommandIdentity,
    presentation: Option<CommandPresentationEnvelope>,
) -> CommandResultEnvelope<serde_json::Value> {
    match result {
        Ok(data) => envelope_for_data(identity, data.clone(), exit_code, presentation),
        Err(err) => CommandResultEnvelope::<()>::from_error(identity, err, exit_code).into_value(),
    }
}

impl CommandResultEnvelope<()> {
    fn into_value(self) -> CommandResultEnvelope<Value> {
        CommandResultEnvelope {
            schema: self.schema,
            command: self.command,
            operation: self.operation,
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
    identity: &CommandIdentity,
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
            evidence.push(CommandArtifactRef {
                id: format!("{}-result", run.id),
                kind: "command-result".to_string(),
                uri: format!("homeboy://runs/{}/result", run.id),
                semantic_key: Some("command_result".to_string()),
            });
        }
    }

    let diagnostics = failure_diagnostics_for_data(exit_code, &run, &artifacts, &data);
    let failure_next_actions = diagnostics
        .as_ref()
        .and_then(|diagnostics| diagnostics.failure_digest.as_ref())
        .map(|digest| digest.next_actions.clone())
        .unwrap_or_default();
    let summary = diagnostics
        .as_ref()
        .map(|diagnostics| diagnostics.message.clone())
        .or_else(|| summary_for_payload(&data, presentation.as_ref()));
    let next_actions = if actionable.next_actions.is_empty() {
        failure_next_actions
    } else {
        actionable.next_actions
    };

    CommandResultEnvelope {
        schema: COMMAND_RESULT_SCHEMA,
        command: identity.command.clone(),
        operation: identity.operation.clone(),
        success,
        exit_code,
        status: status_for_result(Some(&data), exit_code),
        run,
        refs,
        summary,
        next_actions,
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
    data: &Value,
) -> Option<CommandDiagnostics> {
    if exit_code == 0 {
        return None;
    }
    let failure_digest = failure_digest_for_data(data).or_else(|| {
        run.as_ref()
            .and_then(|run| failure_digest_for_run(&run.id, artifacts))
    });
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
    let next_actions = actions_for_error(err);
    match err.code {
        ErrorCode::RemoteCommandFailed => Some(CommandFailureDigest {
            summary: remote_failure_summary(&err.details),
            stdout_tail: string_at(&err.details, &["stdout"]).map(tail_text),
            stderr_tail: string_at(&err.details, &["stderr"]).map(tail_text),
            artifact_refs: Vec::new(),
            next_actions,
            retryable: err.retryable,
        }),
        _ => Some(CommandFailureDigest {
            summary: err.message.clone(),
            stdout_tail: None,
            stderr_tail: None,
            artifact_refs: Vec::new(),
            next_actions,
            retryable: err.retryable,
        }),
    }
}

fn actions_for_error(err: &Error) -> Vec<CommandNextAction> {
    err.details
        .get(ACTIONS_DETAILS_KEY)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| serde_json::from_value::<ExecutableAction>(value.clone()).ok())
        .map(CommandNextAction::from_action)
        .collect()
}

fn failure_digest_for_data(data: &Value) -> Option<CommandFailureDigest> {
    if let Some(digest) = release_failure_digest(data) {
        return Some(digest);
    }
    if let Some(digest) = formatting_failure_digest(data) {
        return Some(digest);
    }
    let failure = data.get("failure").and_then(Value::as_object);
    let summary = failure
        .and_then(|failure| string_at_object(failure, "summary"))
        .or_else(|| {
            data.get("phase")
                .and_then(Value::as_object)
                .and_then(|phase| string_at_object(phase, "summary"))
        })
        .or_else(|| {
            data.get("summary")
                .and_then(Value::as_str)
                .map(str::to_string)
        })?;

    Some(CommandFailureDigest {
        summary,
        stdout_tail: raw_output_tail(data, "stdout_tail"),
        stderr_tail: raw_output_tail(data, "stderr_tail"),
        artifact_refs: Vec::new(),
        next_actions: Vec::new(),
        retryable: None,
    })
}

/// Lift the first failed release step into the bounded command envelope. The
/// complete plan and step payload remain available under `data` for inspection.
fn release_failure_digest(data: &Value) -> Option<CommandFailureDigest> {
    if data.get("command").and_then(Value::as_str) != Some("release") {
        return None;
    }

    let result = data.get("result")?;
    let component_id = result.get("component_id").and_then(Value::as_str)?;
    let steps = result.get("run")?.get("result")?.get("steps")?.as_array()?;
    let failed_step = steps.iter().find(|step| {
        matches!(
            step.get("status").and_then(Value::as_str),
            Some("failed") | Some("missing")
        )
    })?;
    let step_id = failed_step
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let step_type = failed_step
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or(step_id);
    let cause = failed_step
        .get("error")
        .and_then(Value::as_str)
        .or_else(|| {
            failed_step
                .get("missing")
                .and_then(Value::as_array)
                .and_then(|missing| missing.first())
                .and_then(Value::as_str)
        })
        .or_else(|| {
            failed_step
                .get("warnings")
                .and_then(Value::as_array)
                .and_then(|warnings| warnings.first())
                .and_then(Value::as_str)
        })
        .unwrap_or("release step failed without a reported error");

    // The step already classified itself (`reason: "gh-upload-failed"`) and
    // already computed the repair command. Dropping both here is what left CI
    // printing `Release failed: Unknown error` while holding the exact fix
    // (#10441), so lift the classification into the summary and the repair
    // commands into next_actions.
    let step_data = failed_step.get("data");
    let cause = match step_data
        .and_then(|data| data.get("reason"))
        .and_then(Value::as_str)
        .filter(|reason| !reason.trim().is_empty())
    {
        Some(reason) => format!("{reason} — {cause}"),
        None => cause.to_string(),
    };

    let mut next_actions = release_repair_actions(step_data);
    if next_actions.is_empty() {
        next_actions.push(release_gate_repro_action(step_id, step_type, component_id));
    }

    Some(CommandFailureDigest {
        summary: format!(
            "Release step {step_id} ({step_type}) failed: {}",
            bounded_text(&cause, 1000)
        ),
        stdout_tail: None,
        stderr_tail: None,
        artifact_refs: Vec::new(),
        next_actions,
        retryable: None,
    })
}

/// Lift a failed release step's own `repair` block into executable next
/// actions.
///
/// A publish failure is not a quality-gate failure: re-running a gate does
/// nothing for it, and the plan-only `--dry-run` fallback actively misleads.
/// The step already knows whether the GitHub Release object exists, which
/// decides the repair: an existing Draft must be *finished* (upload + publish),
/// never re-created, or the release ends up duplicated.
fn release_repair_actions(step_data: Option<&Value>) -> Vec<CommandNextAction> {
    let Some(repair) = step_data
        .and_then(|data| data.get("repair"))
        .and_then(Value::as_object)
    else {
        return Vec::new();
    };

    let command = |key: &str| {
        repair
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };

    let release_created = step_data
        .and_then(|data| data.get("release_created"))
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut actions = Vec::new();

    if release_created {
        if let Some(upload) = command("upload_command") {
            actions.push(
                CommandNextAction::new(
                    "attach the built artifacts to the release that already exists",
                    upload,
                )
                .with_kind(CommandNextActionKind::Repair),
            );
        }
        if let Some(publish) = command("publish_command") {
            actions.push(
                CommandNextAction::new("publish that release once its assets verify", publish)
                    .with_kind(CommandNextActionKind::Repair),
            );
        }
    } else if let Some(create) = command("create_command") {
        actions.push(
            CommandNextAction::new(
                "create the GitHub Release from the pushed tag and built artifacts (no new tag)",
                create,
            )
            .with_kind(CommandNextActionKind::Repair),
        );
    }

    if let Some(view) = command("view_command").filter(|_| !actions.is_empty()) {
        actions.push(
            CommandNextAction::new("verify the release and its assets", view)
                .with_kind(CommandNextActionKind::Repair),
        );
    }

    actions
}

/// Map a failed release step to a non-mutating command that actually executes
/// that gate.
///
/// `release --dry-run` only renders a plan: it reports quality gates as `ready`
/// without running them, so recommending it after a gate failure returns
/// success while the blocker is untouched (#10114). Point each gate at the
/// surface that can reproduce it instead, and when no such surface exists say
/// plainly that the fallback only inspects the plan.
fn release_gate_repro_action(
    step_id: &str,
    step_type: &str,
    component_id: &str,
) -> CommandNextAction {
    let gate = |value: &str| {
        value
            .strip_prefix("preflight.")
            .unwrap_or(value)
            .to_string()
    };
    let gate_name = {
        let by_id = gate(step_id);
        if by_id == step_id && step_type != step_id {
            gate(step_type)
        } else {
            by_id
        }
    };

    let review_gate = match gate_name.as_str() {
        "lint" => Some("lint"),
        "test" => Some("test"),
        "audit" => Some("audit"),
        // Package and build-structure validation are reproduced by the local
        // build quality gate rather than by re-running the release.
        "package" | "build" => Some("build"),
        _ => None,
    };

    match review_gate {
        Some(gate) => CommandNextAction::new(
            format!("reproduce the failed {gate} gate"),
            format!("homeboy review {gate} {component_id}"),
        )
        .with_kind(CommandNextActionKind::Repair),
        None => CommandNextAction::new(
            "inspect release plan (plan only — does not run quality gates)",
            format!("homeboy release {component_id} --dry-run"),
        )
        .with_kind(CommandNextActionKind::Repair),
    }
}

fn bounded_text(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let bounded: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{bounded}...")
    } else {
        bounded
    }
}

fn formatting_failure_digest(data: &Value) -> Option<CommandFailureDigest> {
    let formatting = find_object_key(data, "formatting_findings")?;
    let files = formatting_files(formatting);
    let command = string_at_object(formatting, "suggested_command")?;
    let summary = if files.is_empty() {
        format!("FORMAT: format check failed; run `{command}`")
    } else {
        format!(
            "FORMAT: format check found {} unformatted file(s): {}; run `{}`",
            files.len(),
            files.join(", "),
            command
        )
    };
    Some(CommandFailureDigest {
        summary,
        stdout_tail: raw_output_tail(data, "stdout_tail"),
        stderr_tail: raw_output_tail(data, "stderr_tail"),
        artifact_refs: Vec::new(),
        next_actions: vec![CommandNextAction::new("fix formatting", command)
            .with_kind(CommandNextActionKind::Repair)],
        retryable: Some(false),
    })
}

fn find_object_key<'a>(value: &'a Value, key: &str) -> Option<&'a Map<String, Value>> {
    match value {
        Value::Object(map) => map
            .get(key)
            .and_then(Value::as_object)
            .or_else(|| map.values().find_map(|value| find_object_key(value, key))),
        Value::Array(values) => values.iter().find_map(|value| find_object_key(value, key)),
        _ => None,
    }
}

fn formatting_files(formatting: &Map<String, Value>) -> Vec<String> {
    formatting
        .get("files")
        .and_then(Value::as_array)
        .map(|files| {
            files
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn string_at_object(object: &Map<String, Value>, key: &str) -> Option<String> {
    object.get(key).and_then(Value::as_str).map(str::to_string)
}

fn raw_output_tail(data: &Value, key: &str) -> Option<String> {
    data.get("raw_output")
        .and_then(Value::as_object)
        .and_then(|raw| string_at_object(raw, key))
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
    let payload_status = data
        .and_then(|value| {
            value
                .get("status")
                .and_then(Value::as_str)
                .or_else(|| value.pointer("/batch/state").and_then(Value::as_str))
        })
        .and_then(normalize_status);
    if exit_code != 0 {
        // A partial result is still a command failure, but retaining its precise
        // aggregate state lets machine consumers distinguish it from all-failed.
        if matches!(
            payload_status,
            Some("partial_failure" | "cancelled" | "timed_out")
        ) {
            return payload_status
                .expect("matched canonical status")
                .to_string();
        }
        return "failed".to_string();
    }

    payload_status.unwrap_or("succeeded").to_string()
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
    write_json_to_file_for_identity(
        result,
        path,
        exit_code,
        &CommandIdentity::top_level(command),
        presentation,
    );
}

pub fn write_json_to_file_for_identity(
    result: &Result<serde_json::Value>,
    path: &str,
    exit_code: i32,
    identity: &CommandIdentity,
    presentation: Option<CommandPresentationEnvelope>,
) {
    let response =
        cli_response_for_json_result_for_identity(result, exit_code, identity, presentation);

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

    /// #10310 collapsed `CommandEvidenceRef` onto `CommandArtifactRef`. They
    /// were the same four fields with the same serde attributes; only the
    /// envelope field they landed in ever distinguished them. Both lists must
    /// still serialize with the identical element shape.
    #[test]
    fn actionable_artifacts_and_evidence_share_one_serialized_element_shape() {
        let metadata = CommandActionableMetadata {
            artifacts: vec![CommandArtifactRef {
                id: "summary".to_string(),
                kind: "artifact".to_string(),
                uri: "homeboy://runs/run-1/artifact/summary".to_string(),
                semantic_key: Some("run.summary".to_string()),
            }],
            evidence: vec![CommandArtifactRef {
                id: "summary".to_string(),
                kind: "artifact".to_string(),
                uri: "homeboy://runs/run-1/artifact/summary".to_string(),
                semantic_key: Some("run.summary".to_string()),
            }],
            ..Default::default()
        };

        let value = serde_json::to_value(&metadata).expect("serialize actionable metadata");
        assert_eq!(value["artifacts"][0], value["evidence"][0]);
        assert_eq!(
            value["artifacts"][0],
            json!({
                "id": "summary",
                "kind": "artifact",
                "uri": "homeboy://runs/run-1/artifact/summary",
                "semantic_key": "run.summary"
            })
        );
    }

    /// `semantic_key` stays omitted rather than serialized as `null`, so
    /// pre-collapse payloads compare equal.
    #[test]
    fn actionable_ref_omits_absent_semantic_key() {
        let value = serde_json::to_value(CommandArtifactRef {
            id: "log".to_string(),
            kind: "log".to_string(),
            uri: "homeboy://runs/run-1/artifact/log".to_string(),
            semantic_key: None,
        })
        .expect("serialize artifact ref");

        assert_eq!(
            value,
            json!({
                "id": "log",
                "kind": "log",
                "uri": "homeboy://runs/run-1/artifact/log"
            })
        );
    }

    #[test]
    fn preflight_failures_keep_resolved_nested_command_identity() {
        let identity = CommandIdentity::with_operation("agent-task", "cook");
        for failure in [
            "destination is dirty",
            "destination has unpushed commits",
            "base does not resolve",
            "destination worktree is missing",
        ] {
            let error = Error::validation_invalid_argument("to_worktree", failure, None, None);
            let result = cli_response_for_json_result_for_identity(&Err(error), 2, &identity, None);
            let value = serde_json::to_value(result).expect("serialize result");
            assert_eq!(value["command"], "agent-task", "{failure}");
            assert_eq!(value["operation"], "cook", "{failure}");
        }
    }

    fn release_failure_payload(step_id: &str, step_type: &str) -> Value {
        json!({
            "command": "release",
            "result": {
                "component_id": "fixture",
                "run": { "result": { "steps": [
                    { "id": step_id, "type": step_type, "status": "failed", "error": "gate failed" }
                ] } }
            }
        })
    }

    /// #10114: a failed quality gate must hand back a command that actually
    /// runs that gate. `release --dry-run` reports gates as `ready` without
    /// executing them, so recommending it returns success while the blocker
    /// is unchanged.
    #[test]
    fn release_gate_failures_recommend_the_gate_that_can_reproduce_them() {
        for (step, expected) in [
            ("preflight.lint", "homeboy review lint fixture"),
            ("preflight.test", "homeboy review test fixture"),
            ("preflight.audit", "homeboy review audit fixture"),
            ("preflight.package", "homeboy review build fixture"),
        ] {
            let digest = release_failure_digest(&release_failure_payload(step, step))
                .expect("release digest");
            let action = digest.next_actions.first().expect("next action");
            assert_eq!(
                action.command, expected,
                "{step} should be reproducible via its own gate"
            );
            assert!(
                !action.command.contains("--dry-run"),
                "{step} must not recommend a plan-only dry run"
            );
        }
    }

    /// Steps with no non-mutating reproduction surface still fall back to the
    /// plan, but the label must not imply the gates were verified.
    #[test]
    fn non_gate_release_failures_label_dry_run_as_plan_only() {
        let digest = release_failure_digest(&release_failure_payload("git.push", "git.push"))
            .expect("release digest");
        let action = digest.next_actions.first().expect("next action");

        assert_eq!(action.command, "homeboy release fixture --dry-run");
        assert!(
            action.label.contains("plan only"),
            "dry-run fallback must be labeled plan-only, got: {}",
            action.label
        );
    }

    fn github_release_failure_payload(step_data: Value) -> Value {
        json!({
            "command": "release",
            "result": {
                "component_id": "homeboy",
                "run": { "result": { "steps": [{
                    "id": "github.release",
                    "type": "github.release",
                    "status": "failed",
                    "error": "`gh release upload` failed for v0.320.0: gh api release metadata exited with status 1",
                    "data": step_data
                }] } }
            }
        })
    }

    /// #10441: the step classified itself (`reason`) and computed the repair
    /// command, then the envelope threw both away — which is how CI ended up
    /// printing "Release failed: Unknown error" while holding the exact fix.
    #[test]
    fn failed_publish_surfaces_its_classified_reason_and_repair_commands() {
        let digest = release_failure_digest(&github_release_failure_payload(json!({
            "reason": "gh-upload-failed",
            "release_created": true,
            "repair": {
                "upload_command": "gh release upload 'v0.320.0' 'a.tar.gz' --clobber -R 'Extra-Chill/homeboy'",
                "publish_command": "gh release edit 'v0.320.0' --draft=false -R 'Extra-Chill/homeboy'",
                "create_command": "gh release create 'v0.320.0' --title 'v0.320.0'",
                "view_command": "gh release view 'v0.320.0' -R 'Extra-Chill/homeboy'"
            }
        })))
        .expect("release digest");

        assert!(
            digest.summary.contains("gh-upload-failed"),
            "classified reason must reach the envelope summary, got: {}",
            digest.summary
        );

        let commands: Vec<&str> = digest
            .next_actions
            .iter()
            .map(|action| action.command.as_str())
            .collect();
        assert_eq!(
            commands,
            vec![
                "gh release upload 'v0.320.0' 'a.tar.gz' --clobber -R 'Extra-Chill/homeboy'",
                "gh release edit 'v0.320.0' --draft=false -R 'Extra-Chill/homeboy'",
                "gh release view 'v0.320.0' -R 'Extra-Chill/homeboy'",
            ],
            "an existing release must be finished, never re-created"
        );
        assert!(
            !commands.iter().any(|command| command.contains("--dry-run")),
            "a publish failure must not be handed a plan-only dry run"
        );
    }

    /// When no GitHub Release object exists yet, the repair is to create it
    /// from the already-pushed tag — not to cut a second tag.
    #[test]
    fn failed_publish_without_a_release_object_recommends_creating_it() {
        let digest = release_failure_digest(&github_release_failure_payload(json!({
            "reason": "gh-create-failed",
            "release_created": false,
            "repair": {
                "create_command": "gh release create 'v0.320.0' --title 'v0.320.0' --notes-file 'notes.md'",
                "upload_command": "gh release upload 'v0.320.0' --clobber",
                "view_command": "gh release view 'v0.320.0'"
            }
        })))
        .expect("release digest");

        let first = digest.next_actions.first().expect("next action");
        assert_eq!(
            first.command,
            "gh release create 'v0.320.0' --title 'v0.320.0' --notes-file 'notes.md'"
        );
        assert!(digest.summary.contains("gh-create-failed"));
    }

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
    fn actionable_errors_lift_one_explicit_repair_contract_into_human_and_machine_forms() {
        let action = ExecutableAction::new(
            "runner.disconnect",
            "disconnect runner lab one",
            "homeboy",
            ["runner", "disconnect", "lab one"],
            homeboy::core::error::ActionSafety::Mutating,
        )
        .requiring_confirmation("operator")
        .with_evidence(json!({ "runner_id": "lab one", "lease_id": "lease-123" }));
        let response = cli_response_for_json_result_for_command(
            &Err(
                Error::validation_invalid_argument("runner", "stale daemon", None, None)
                    .with_action(action),
            ),
            2,
            "agent-task",
            None,
        );
        let value = serde_json::to_value(response).expect("response json");
        let action = &value["next_actions"][0];

        assert_eq!(action["label"], "disconnect runner lab one");
        assert_eq!(action["kind"], "repair");
        assert_eq!(action["command"], "homeboy runner disconnect 'lab one'");
        assert_eq!(action["action"]["id"], "runner.disconnect");
        assert_eq!(action["action"]["program"], "homeboy");
        assert_eq!(
            action["action"]["args"],
            json!(["runner", "disconnect", "lab one"])
        );
        assert_eq!(action["action"]["safety"], "mutating");
        assert_eq!(
            action["action"]["required_confirmations"],
            json!(["operator"])
        );
        assert_eq!(action["action"]["evidence"]["lease_id"], "lease-123");
        assert_eq!(
            value["diagnostics"]["failure_digest"]["next_actions"][0]["command"],
            action["command"]
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
    fn failed_quality_payload_includes_format_failure_digest_without_run_evidence() {
        let response = cli_response_for_json_result_for_command(
            &Ok(json!({
                "command": "review",
                "summary": { "status": "failed" },
                "lint": {
                    "stage": "lint",
                    "passed": false,
                    "output": {
                        "formatting_findings": {
                            "files": ["src/lib.rs", "src/main.rs"],
                            "suggested_command": "cargo fmt"
                        }
                    }
                }
            })),
            1,
            "review",
            None,
        );
        let value = serde_json::to_value(response).expect("response json");

        assert_eq!(value["success"], false);
        assert_eq!(
            value["diagnostics"]["failure_digest"]["summary"],
            "FORMAT: format check found 2 unformatted file(s): src/lib.rs, src/main.rs; run `cargo fmt`"
        );
        assert_eq!(
            value["diagnostics"]["failure_digest"]["next_actions"][0]["command"],
            "cargo fmt"
        );
    }

    #[test]
    fn failed_release_lifts_a_bounded_root_cause_and_retry_action() {
        let long_error = format!("package upload rejected: {}", "x".repeat(1_100));
        let response = cli_response_for_json_result_for_command(
            &Ok(json!({
                "command": "release",
                "variant": "single",
                "result": {
                    "component_id": "site-generator",
                    "run": {
                        "result": {
                            "steps": [{
                                "id": "publish.npm",
                                "type": "publish.npm",
                                "status": "failed",
                                "error": long_error,
                                "data": { "verbose_log": "retained in the detailed release output" }
                            }]
                        }
                    }
                }
            })),
            1,
            "release",
            None,
        );
        let value = serde_json::to_value(response).expect("response json");
        let summary = value["summary"].as_str().expect("bounded root cause");

        assert!(summary.starts_with("Release step publish.npm (publish.npm) failed:"));
        assert!(summary.ends_with("..."));
        assert!(summary.chars().count() <= 1_050);
        assert_eq!(value["diagnostics"]["message"], summary);
        assert_eq!(
            value["next_actions"][0]["command"],
            "homeboy release site-generator --dry-run"
        );
        assert_eq!(
            value["data"]["result"]["run"]["result"]["steps"][0]["data"]["verbose_log"],
            "retained in the detailed release output"
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
