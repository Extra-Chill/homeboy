//! Runner-exec run identity, generic runner-exec plan construction, and Lab
//! offload submission-intent/request recording. Extracted from `lifecycle_ops`
//! to keep that module within the god-file threshold (#9927).

use serde_json::{json, Value};

use homeboy_core::api_jobs::RemoteRunnerJobRequest;

use super::*;

pub fn record_runner_job_identity(
    run_id: &str,
    runner_id: &str,
    runner_job_id: &str,
) -> Result<AgentTaskRunRecord> {
    let mut record = store::read_record(&sanitize_run_id(run_id))?;
    let metadata = record.ensure_metadata_object();
    metadata.insert("runner_id".to_string(), json!(runner_id));
    metadata.insert("runner_job_id".to_string(), json!(runner_job_id));
    store::write_record(&record)?;
    Ok(record)
}

/// Read the accepted runner binding without triggering status reconciliation.
/// Controller-owned cancellation uses this durable parent projection to close
/// the window between child acceptance and its next controller checkpoint.
pub fn recorded_runner_job_identity(run_id: &str) -> Result<Option<(String, String)>> {
    let record = store::read_record(&sanitize_run_id(run_id))?;
    Ok(record
        .runner_id()
        .zip(record.runner_job_id())
        .map(|(runner_id, runner_job_id)| (runner_id.to_string(), runner_job_id.to_string())))
}

/// Read only a typed, daemon-authoritative accepted Lab handoff. Unlike the
/// compatibility metadata projection, this cannot be forged by mutating a run
/// record's `runner_id` or `runner_job_id` fields.
pub fn accepted_lab_runner_job_identity(
    run_id: &str,
) -> Result<Option<homeboy_core::lab_contract::RunnerJobIdentity>> {
    let record = store::read_record(&sanitize_run_id(run_id))?;
    let Some(handoff) = record.lab_handoff.as_ref().filter(|handoff| {
        handoff.validation_error().is_none()
            && handoff.state == AgentTaskLabHandoffState::Accepted
            && handoff.authority == AgentTaskLabHandoffAuthority::RunnerDaemon
    }) else {
        return Ok(None);
    };
    let identity = homeboy_core::lab_contract::RunnerJobIdentity::new(
        &record.run_id,
        &handoff.runner_id,
        handoff.runner_job_id.clone().unwrap_or_default(),
    );
    Ok(identity.is_complete().then_some(identity))
}

/// Metadata `kind` marker for a generic runner-execution run. It distinguishes
/// an ad hoc `runner exec --run-id` durable run from an agent-task lifecycle
/// record so ownership collisions are detectable (#8447).
pub const RUNNER_EXEC_RUN_KIND: &str = "runner_exec";

fn record_run_kind(record: &AgentTaskRunRecord) -> Option<&str> {
    record.metadata.get("kind").and_then(Value::as_str)
}

/// Bind a runner job to an ad hoc `runner exec --run-id` identity. Unlike
/// [`record_runner_job_identity`], this owns a *generic* runner-execution run:
/// a caller-supplied ID that has no prior record creates one on demand rather
/// than failing closed as a missing agent-task record. Reusing an ID that is
/// already owned by an agent-task lifecycle run fails before runner mutation
/// with an explicit ownership diagnostic (#8447).
pub fn record_runner_exec_job_identity(
    run_id: &str,
    runner_id: &str,
    runner_job_id: &str,
    remote_workspace: &str,
    remote_command: &[String],
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let mut record = match store::read_record(&run_id) {
        Ok(record) => {
            // An existing record must be a generic runner-exec run. An agent-task
            // record with the same ID is a different owner: fail closed rather
            // than mutating it under generic runner-exec semantics.
            match record_run_kind(&record) {
                Some(RUNNER_EXEC_RUN_KIND) => record,
                other => {
                    return Err(Error::validation_invalid_argument(
                        "run_id",
                        format!(
                            "run '{run_id}' already exists as {} and cannot be reused as a generic runner-exec run",
                            other
                                .map(|kind| format!("an agent-task run (kind '{kind}')"))
                                .unwrap_or_else(|| "an agent-task run".to_string())
                        ),
                        Some(run_id.clone()),
                        Some(vec![
                            "Pass a distinct --run-id for ad hoc runner exec evidence.".to_string(),
                        ]),
                    ));
                }
            }
        }
        Err(error) if error.code == ErrorCode::ValidationInvalidArgument => submit_plan(
            &runner_exec_plan(&run_id, runner_id, remote_workspace, remote_command),
            Some(&run_id),
        )?,
        Err(error) => return Err(error),
    };
    let metadata = record.ensure_metadata_object();
    metadata.insert("kind".to_string(), json!(RUNNER_EXEC_RUN_KIND));
    metadata.insert("runner_id".to_string(), json!(runner_id));
    metadata.insert("runner_job_id".to_string(), json!(runner_job_id));
    store::write_record(&record)?;
    Ok(record)
}

/// Create (or validate ownership of) a generic runner-exec run that has no
/// daemon runner job — the diagnostic-SSH transport executes synchronously and
/// never accepts a durable runner job, but a caller-supplied `--run-id` with
/// declared `--artifact`/`--artifact-dir`/`--summary` still needs a persisted
/// run to attach that evidence to. Mirrors [`record_runner_exec_job_identity`]'s
/// on-demand creation and fail-closed ownership check, minus the job binding
/// (Extra-Chill/homeboy#9485, restoring #8447 for the SSH path).
pub fn ensure_generic_runner_exec_run(
    run_id: &str,
    runner_id: &str,
    remote_workspace: &str,
    remote_command: &[String],
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let mut record = match store::read_record(&run_id) {
        Ok(record) => match record_run_kind(&record) {
            Some(RUNNER_EXEC_RUN_KIND) => record,
            other => {
                return Err(Error::validation_invalid_argument(
                    "run_id",
                    format!(
                        "run '{run_id}' already exists as {} and cannot be reused as a generic runner-exec run",
                        other
                            .map(|kind| format!("an agent-task run (kind '{kind}')"))
                            .unwrap_or_else(|| "an agent-task run".to_string())
                    ),
                    Some(run_id.clone()),
                    Some(vec![
                        "Pass a distinct --run-id for ad hoc runner exec evidence.".to_string(),
                    ]),
                ));
            }
        },
        Err(error) if error.code == ErrorCode::ValidationInvalidArgument => submit_plan(
            &runner_exec_plan(&run_id, runner_id, remote_workspace, remote_command),
            Some(&run_id),
        )?,
        Err(error) => return Err(error),
    };
    let metadata = record.ensure_metadata_object();
    metadata.insert("kind".to_string(), json!(RUNNER_EXEC_RUN_KIND));
    metadata.insert("runner_id".to_string(), json!(runner_id));
    store::write_record(&record)?;
    Ok(record)
}

fn runner_exec_plan(
    run_id: &str,
    runner_id: &str,
    remote_workspace: &str,
    remote_command: &[String],
) -> AgentTaskPlan {
    let task = AgentTaskRequest {
        schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
        task_id: format!("{run_id}-runner-exec"),
        group_key: Some("runner-exec".to_string()),
        parent_plan_id: None,
        executor: AgentTaskExecutor {
            backend: "homeboy-lab".to_string(),
            selector: Some(runner_id.to_string()),
            runtime_selection: None,
            required_capabilities: Vec::new(),
            secret_env: Vec::new(),
            model: None,
            config: Value::Null,
        },
        instructions: "Ad hoc runner exec evidence bound to a generic runner-execution run."
            .to_string(),
        inputs: json!({
            "runner_id": runner_id,
            "remote_workspace": remote_workspace,
            "remote_command": remote_command,
        }),
        source_refs: vec![AgentTaskSourceRef {
            kind: "runner-exec".to_string(),
            uri: format!("homeboy://runner/{runner_id}/exec/{run_id}"),
            revision: None,
        }],
        workspace: AgentTaskWorkspace {
            mode: AgentTaskWorkspaceMode::Existing,
            root: Some(remote_workspace.to_string()),
            kind: Some("runner-exec".to_string()),
            cleanup: Some("preserve".to_string()),
            materialization: json!({ "runner_id": runner_id }),
            ..AgentTaskWorkspace::default()
        },
        component_contracts: Vec::new(),
        policy: AgentTaskPolicy::default(),
        limits: AgentTaskLimits::default(),
        expected_artifacts: Vec::new(),
        artifact_declarations: Vec::new(),
        metadata: json!({
            "kind": RUNNER_EXEC_RUN_KIND,
            "runner_id": runner_id,
        }),
    };
    let mut plan = AgentTaskPlan::new(format!("{run_id}-runner-exec"), vec![task]);
    plan.group_key = Some("runner-exec".to_string());
    plan.metadata = json!({
        "kind": RUNNER_EXEC_RUN_KIND,
        "runner_id": runner_id,
        "remote_workspace": remote_workspace,
    });
    plan
}

/// Persist redacted submission ownership before a reverse-broker POST. The
/// command itself is canonical controller provenance; secret values are never
/// copied here, only the names the runner must hydrate at dispatch.
pub fn record_lab_offload_submission_intent(
    run_id: &str,
    runner_id: &str,
    remote_workspace: &str,
    remote_command: &[String],
    secret_env_names: &[String],
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let _lock = LabHandoffLock::lock(&run_id)?;
    let mut record = store::read_record(&run_id)?;
    let submission_key = format!("agent-task:v1:{runner_id}:{run_id}");
    if let Some(handoff) = record.lab_handoff.as_mut() {
        handoff.submission_key = Some(submission_key.clone());
        handoff.payload_fingerprint = None;
    }
    let metadata = record.ensure_metadata_object();
    metadata.insert(
        "runner_submission_intent".to_string(),
        json!({
            "state": "preparing",
            "submission_key": submission_key,
            "runner_id": runner_id,
            "ordering": "broker_fifo",
            "eligibility": "reverse_runner_detached_durable_handoff",
            "canonical_workload": {
                "run_id": run_id,
                "remote_workspace": remote_workspace,
                "remote_command": remote_command,
            },
            "secret_env_names": secret_env_names,
        }),
    );
    metadata.insert("phase".to_string(), json!("waiting_for_runner_capacity"));
    metadata.insert(
        "phase_activity".to_string(),
        json!("durable broker submission intent recorded; waiting for runner capacity"),
    );
    store::write_record(&record)?;
    Ok(record)
}

/// Replace a preflight intent with the exact normalized, redacted request that
/// will cross the broker boundary. This is the final durable write before POST.
pub fn record_lab_offload_submission_request(
    run_id: &str,
    request: &RemoteRunnerJobRequest,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let _lock = LabHandoffLock::lock(&run_id)?;
    let mut record = store::read_record(&run_id)?;
    if record.state.is_terminal() {
        return Ok(record);
    }
    let submission_key = request.submission_key().ok_or_else(|| {
        Error::internal_unexpected("Lab runner submission request has no stable submission key")
    })?;
    let replay_request = request.redacted_for_durable_replay();
    let payload_fingerprint = replay_request.submission_payload_fingerprint()?;
    let now = chrono::Utc::now();
    let mut handoff = AgentTaskLabHandoff::pending(
        &replay_request.runner_id,
        now.to_rfc3339(),
        (now + chrono::Duration::seconds(lab_handoff_acceptance_timeout_seconds())).to_rfc3339(),
    );
    handoff.submission_key = Some(submission_key.to_string());
    handoff.payload_fingerprint = Some(payload_fingerprint.clone());
    record.lab_handoff = Some(handoff.clone());
    let metadata = record.ensure_metadata_object();
    metadata.insert(
        "runner_submission_intent".to_string(),
        json!({
            "state": "pending",
            "submission_key": submission_key,
            "payload_fingerprint": payload_fingerprint,
            "runner_id": replay_request.runner_id,
            "replay_request": replay_request,
        }),
    );
    metadata.insert(
        "handoff_acceptance".to_string(),
        json!({
            "state": "pending",
            "started_at": handoff.submitted_at,
            "deadline_at": handoff.acceptance_deadline_at,
        }),
    );
    store::write_record(&record)?;
    Ok(record)
}
