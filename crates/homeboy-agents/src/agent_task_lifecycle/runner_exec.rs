//! Runner-exec run identity, generic runner-exec plan construction, and Lab
//! offload submission-intent/request recording. Extracted from `lifecycle_ops`
//! to keep that module within the god-file threshold (#9927).

use serde_json::{json, Value};

use homeboy_core::api_jobs::{JobStatus, RemoteRunnerJobRequest, RunnerJobLogSnapshot};
use homeboy_core::observation::RunStatus;

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

const RUNNER_EXEC_OBSERVATION_KIND: &str = "runner_execution";

fn ensure_runner_exec_observation_run(
    run_id: &str,
    runner_id: &str,
    remote_workspace: &str,
    remote_command: &[String],
    runner_job_id: Option<&str>,
) -> Result<homeboy_core::observation::RunRecord> {
    let run_id = sanitize_run_id(run_id);
    let store = homeboy_core::observation::ObservationStore::open_initialized()?;
    let mut run = match store.get_run(&run_id)? {
        Some(run)
            if run.metadata_json.get("kind").and_then(Value::as_str)
                == Some(RUNNER_EXEC_RUN_KIND) =>
        {
            run
        }
        Some(run) => {
            return Err(Error::validation_invalid_argument(
                "run_id",
                format!(
                    "run '{run_id}' already exists as {} and cannot be reused as a generic runner-exec run",
                    if run.kind == "agent-task" {
                        "an agent-task run".to_string()
                    } else {
                        format!("a {} run", run.kind)
                    }
                ),
                Some(run_id),
                Some(vec![
                    "Pass a distinct --run-id for ad hoc runner exec evidence.".to_string(),
                ]),
            ));
        }
        None => homeboy_core::observation::RunRecord {
            id: run_id,
            kind: RUNNER_EXEC_OBSERVATION_KIND.to_string(),
            component_id: Some(runner_id.to_string()),
            started_at: now_timestamp(),
            finished_at: None,
            status: homeboy_core::observation::RunStatus::Running
                .as_str()
                .to_string(),
            command: Some(remote_command.join(" ")),
            cwd: Some(remote_workspace.to_string()),
            homeboy_version: Some(homeboy_core::build_identity::current().version),
            git_sha: None,
            rig_id: None,
            metadata_json: json!({}),
        },
    };

    if !run.metadata_json.is_object() {
        run.metadata_json = json!({ "homeboy_original_metadata": run.metadata_json });
    }
    let metadata = run.metadata_json.as_object_mut().expect("metadata object");
    metadata.insert("kind".to_string(), json!(RUNNER_EXEC_RUN_KIND));
    metadata.insert("runner_id".to_string(), json!(runner_id));
    metadata.insert("remote_workspace".to_string(), json!(remote_workspace));
    metadata.insert("remote_command".to_string(), json!(remote_command));
    if let Some(runner_job_id) = runner_job_id {
        metadata.insert("runner_job_id".to_string(), json!(runner_job_id));
    }
    store.upsert_imported_run_preserving_terminal(&run)?;
    Ok(run)
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
) -> Result<homeboy_core::observation::RunRecord> {
    ensure_runner_exec_observation_run(
        run_id,
        runner_id,
        remote_workspace,
        remote_command,
        Some(runner_job_id),
    )
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
) -> Result<homeboy_core::observation::RunRecord> {
    ensure_runner_exec_observation_run(run_id, runner_id, remote_workspace, remote_command, None)
}

/// Persist the output contract before submitting a daemon job. This gives a
/// restarted controller the exact runner-side paths it must retain, rather than
/// relying on ephemeral CLI arguments after the daemon has completed.
pub fn record_runner_exec_artifact_declarations(
    run_id: &str,
    artifacts: &[String],
    artifact_dirs: &[String],
    summaries: &[String],
) -> Result<()> {
    let store = homeboy_core::observation::ObservationStore::open_initialized()?;
    let run_id = sanitize_run_id(run_id);
    let Some(mut run) = store.get_run(&run_id)? else {
        return Err(Error::validation_invalid_argument(
            "run_id",
            format!("generic runner-exec run record not found: {run_id}"),
            Some(run_id),
            None,
        ));
    };
    if run.metadata_json.get("kind").and_then(Value::as_str) != Some(RUNNER_EXEC_RUN_KIND) {
        return Err(Error::validation_invalid_argument(
            "run_id",
            "runner-exec artifact declarations require a generic runner-exec run",
            Some(run.id),
            None,
        ));
    }
    let metadata = run.metadata_json.as_object_mut().expect("metadata object");
    metadata.insert(
        "runner_exec_artifact_declarations".to_string(),
        json!({
            "artifacts": artifacts,
            "artifact_dirs": artifact_dirs,
            "summaries": summaries,
        }),
    );
    metadata.insert(
        "runner_terminal_projection".to_string(),
        json!({
            "state": "awaiting_daemon_terminal",
            "artifact_promotion": "pending",
        }),
    );
    store.upsert_imported_run_preserving_terminal(&run)
}

/// Checkpoint the authoritative terminal snapshot before copying declared
/// evidence. A restart can resume promotion from this durable boundary instead
/// of terminalizing a run whose runner-side files were never retained.
pub fn record_runner_exec_terminal_checkpoint(
    run_id: &str,
    snapshot: &RunnerJobLogSnapshot,
) -> Result<()> {
    if !snapshot.job.status.is_terminal() {
        return Ok(());
    }
    let store = homeboy_core::observation::ObservationStore::open_initialized()?;
    let run_id = sanitize_run_id(run_id);
    let Some(mut run) = store.get_run(&run_id)? else {
        return Ok(());
    };
    if run.metadata_json.get("kind").and_then(Value::as_str) != Some(RUNNER_EXEC_RUN_KIND) {
        return Ok(());
    }
    validate_runner_exec_snapshot_binding(&run, snapshot)?;
    run.metadata_json
        .as_object_mut()
        .expect("metadata object")
        .insert(
            "runner_terminal_projection".to_string(),
            json!({
                "state": "terminal_checkpointed",
                "artifact_promotion": "pending",
                "job_id": snapshot.job.id,
                "status": snapshot.job.status,
                "event_count": snapshot.events.len(),
            }),
        );
    store.upsert_imported_run_preserving_terminal(&run)
}

/// Retain controller-owned artifact IDs alongside the original declarations.
/// These IDs remain usable after the daemon evicts its job/event retention.
pub fn record_runner_exec_artifact_refs(
    run_id: &str,
    artifacts: &[homeboy_core::observation::ArtifactRecord],
) -> Result<()> {
    let store = homeboy_core::observation::ObservationStore::open_initialized()?;
    let run_id = sanitize_run_id(run_id);
    let Some(mut run) = store.get_run(&run_id)? else {
        return Ok(());
    };
    if run.metadata_json.get("kind").and_then(Value::as_str) != Some(RUNNER_EXEC_RUN_KIND) {
        return Ok(());
    }
    let metadata = run.metadata_json.as_object_mut().expect("metadata object");
    let mut refs = metadata
        .get("runner_exec_artifact_refs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for artifact in artifacts {
        if refs.iter().any(|entry| entry["id"] == artifact.id) {
            continue;
        }
        refs.push(json!({
            "id": artifact.id.clone(),
            "kind": artifact.kind.clone(),
            "path": artifact.path.clone(),
        }));
    }
    let artifact_count = refs.len();
    metadata.insert("runner_exec_artifact_refs".to_string(), Value::Array(refs));
    metadata.insert(
        "runner_terminal_projection".to_string(),
        json!({
            "state": "terminal_checkpointed",
            "artifact_promotion": "complete",
            "artifact_count": artifact_count,
        }),
    );
    store.upsert_imported_run_preserving_terminal(&run)
}

/// Persist one declaration's completed promotion immediately. The artifact IDs
/// and content hashes make a replay skip that declaration after a crash while
/// allowing later declarations to resume independently.
pub fn record_runner_exec_declaration_promotion(
    run_id: &str,
    role: &str,
    declaration: &str,
    artifacts: &[homeboy_core::observation::ArtifactRecord],
) -> Result<()> {
    let store = homeboy_core::observation::ObservationStore::open_initialized()?;
    let run_id = sanitize_run_id(run_id);
    let Some(mut run) = store.get_run(&run_id)? else {
        return Ok(());
    };
    if run.metadata_json.get("kind").and_then(Value::as_str) != Some(RUNNER_EXEC_RUN_KIND) {
        return Ok(());
    }
    let metadata = run.metadata_json.as_object_mut().expect("metadata object");
    let key = format!("{role}:{declaration}");
    let mut states = metadata
        .get("runner_exec_declaration_promotions")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    states.insert(
        key,
        json!({
            "role": role,
            "declaration": declaration,
            "state": "promoted",
            "artifacts": artifacts.iter().map(|artifact| json!({
                "id": artifact.id,
                "sha256": artifact.sha256,
                "path": artifact.path,
            })).collect::<Vec<_>>(),
        }),
    );
    metadata.insert(
        "runner_exec_declaration_promotions".to_string(),
        Value::Object(states),
    );
    store.upsert_imported_run_preserving_terminal(&run)
}

pub fn runner_exec_declaration_is_promoted(
    run: &homeboy_core::observation::RunRecord,
    role: &str,
    declaration: &str,
) -> bool {
    run.metadata_json
        .pointer(&format!(
            "/runner_exec_declaration_promotions/{role}:{declaration}/state"
        ))
        .and_then(Value::as_str)
        == Some("promoted")
}

/// Finalize a generic runner-exec observation from a daemon-owned terminal
/// snapshot. Agent-task projections intentionally remain separate: this record
/// has no task aggregate to parse, so the daemon result itself is authoritative.
pub fn project_terminal_runner_exec_result(
    run_id: &str,
    snapshot: &RunnerJobLogSnapshot,
) -> Result<bool> {
    if !snapshot.job.status.is_terminal() {
        return Ok(false);
    }
    let store = homeboy_core::observation::ObservationStore::open_initialized()?;
    let Some(mut run) = store.get_run(&sanitize_run_id(run_id))? else {
        return Ok(false);
    };
    if run.metadata_json.get("kind").and_then(Value::as_str) != Some(RUNNER_EXEC_RUN_KIND) {
        return Ok(false);
    }
    if RunStatus::from_label(&run.status).is_some_and(RunStatus::is_terminal) {
        return Ok(false);
    }

    let runner_id = validate_runner_exec_snapshot_binding(&run, snapshot)?;
    if run
        .metadata_json
        .pointer("/runner_terminal_projection/artifact_promotion")
        .and_then(Value::as_str)
        == Some("pending")
    {
        return Err(Error::internal_unexpected(
            "runner-exec terminal projection requires declared artifact promotion before finalization",
        ));
    }
    let exit_code = if snapshot.job.status == JobStatus::Succeeded {
        0
    } else {
        1
    };
    let metadata = run.metadata_json.as_object_mut().expect("metadata object");
    metadata.insert("runner_job_id".to_string(), json!(snapshot.job.id));
    metadata.insert("runner_job_status".to_string(), json!(snapshot.job.status));
    metadata.insert("runner_job_events".to_string(), json!(snapshot.events));
    let mut execution_record =
        homeboy_core::runner_execution_envelope::RunnerExecutionRecord::terminal(
            snapshot.job.id.to_string(),
            runner_id,
            "daemon",
            exit_code,
        )
        .with_job_id(snapshot.job.id.to_string());
    if snapshot.job.status == JobStatus::Cancelled {
        execution_record.status = "cancelled".to_string();
    }
    metadata.insert(
        "runner_execution_record".to_string(),
        serde_json::to_value(execution_record).unwrap_or(Value::Null),
    );
    metadata.insert(
        "runner_terminal_projection".to_string(),
        json!({
            "state": "projected",
            "job_id": snapshot.job.id,
            "status": snapshot.job.status,
            "event_count": snapshot.events.len(),
        }),
    );
    if snapshot.job.status != JobStatus::Succeeded {
        metadata.insert(
            "runner_failure_diagnostics".to_string(),
            json!({ "job_status": snapshot.job.status, "events": snapshot.events }),
        );
    }
    let status = if snapshot.job.status == JobStatus::Succeeded {
        RunStatus::Pass
    } else {
        RunStatus::Fail
    };
    store.finish_run(&run.id, status, Some(run.metadata_json))?;
    Ok(true)
}

/// Finish a synchronous transport that has no daemon job identity (diagnostic
/// SSH/local execution) after its declared evidence is safely retained.
pub fn finish_runner_exec_direct(run_id: &str, transport: &str, exit_code: i32) -> Result<bool> {
    let store = homeboy_core::observation::ObservationStore::open_initialized()?;
    let Some(mut run) = store.get_run(&sanitize_run_id(run_id))? else {
        return Ok(false);
    };
    if run.metadata_json.get("kind").and_then(Value::as_str) != Some(RUNNER_EXEC_RUN_KIND)
        || RunStatus::from_label(&run.status).is_some_and(RunStatus::is_terminal)
    {
        return Ok(false);
    }
    let metadata = run.metadata_json.as_object_mut().expect("metadata object");
    metadata.insert(
        "runner_execution_record".to_string(),
        json!({
            "transport": transport, "status": if exit_code == 0 { "succeeded" } else { "failed" },
            "exit_code": exit_code,
        }),
    );
    metadata.insert(
        "runner_terminal_projection".to_string(),
        json!({
            "state": "projected", "transport": transport, "artifact_promotion": "complete",
        }),
    );
    store.finish_run(
        &run.id,
        if exit_code == 0 {
            RunStatus::Pass
        } else {
            RunStatus::Fail
        },
        Some(run.metadata_json),
    )?;
    Ok(true)
}

fn validate_runner_exec_snapshot_binding(
    run: &homeboy_core::observation::RunRecord,
    snapshot: &RunnerJobLogSnapshot,
) -> Result<String> {
    let runner_id = run
        .metadata_json
        .get("runner_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let runner_job_id = run
        .metadata_json
        .get("runner_job_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let snapshot_runner_id = snapshot.job.target_runner_id.as_deref().unwrap_or_default();
    if runner_id.is_empty()
        || runner_job_id.is_empty()
        || runner_id != snapshot_runner_id
        || runner_job_id != snapshot.job.id.to_string()
    {
        return Err(Error::validation_invalid_argument(
            "runner_job_id",
            format!(
                "terminal runner snapshot does not match durable binding ({runner_id}/{runner_job_id})"
            ),
            Some(run.id.clone()),
            None,
        ));
    }
    Ok(runner_id)
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
