//! Runner-exec run identity, generic runner-exec plan construction, and Lab
//! offload submission-intent/request recording. Extracted from `lifecycle_ops`
//! to keep that module within the god-file threshold (#9927).

use serde_json::{json, Value};

#[cfg(test)]
use homeboy_core::api_jobs::RemoteRunnerJobRequest;
use homeboy_core::api_jobs::{JobStatus, RunnerJobLogSnapshot};
use homeboy_core::observation::RunStatus;
use homeboy_core::redaction::redact_argv;

use super::*;

pub fn record_runner_job_identity(
    run_id: &str,
    runner_id: &str,
    runner_job_id: &str,
) -> Result<AgentTaskRunRecord> {
    record_runner_job_identity_in_store(
        &AgentTaskLifecycleStore::from_current_environment()?,
        run_id,
        runner_id,
        runner_job_id,
    )
}

/// The store-rooted counterpart of [`record_runner_job_identity`].
///
/// The read and the write are one operation — the record read back is the one
/// returned to the caller — so they must name the same installation (#7505).
pub fn record_runner_job_identity_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    runner_id: &str,
    runner_job_id: &str,
) -> Result<AgentTaskRunRecord> {
    let mut record = lifecycle_store.read_record(&sanitize_run_id(run_id))?;
    let metadata = record.ensure_metadata_object();
    metadata.insert("runner_id".to_string(), json!(runner_id));
    metadata.insert("runner_job_id".to_string(), json!(runner_job_id));
    lifecycle_store.write_record(&record)?;
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
    accepted_lab_runner_job_identity_in_store(
        &AgentTaskLifecycleStore::from_current_environment()?,
        run_id,
    )
}

/// The store-rooted counterpart of [`accepted_lab_runner_job_identity`].
///
/// This is the read half of Lab acceptance, and `record_detached_lab_run_in_store`
/// is the write half. An acceptance committed into one installation and then
/// verified against another's record would report "not accepted" for a run that
/// is already bound — the mirror image of the double-acceptance the write side
/// refuses — so both halves have to name the same store (#7505).
pub fn accepted_lab_runner_job_identity_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<Option<homeboy_core::lab_contract::RunnerJobIdentity>> {
    let record = lifecycle_store.read_record(&sanitize_run_id(run_id))?;
    Ok(accepted_lab_runner_job_identity_from_record(&record))
}

pub(crate) fn accepted_lab_runner_job_identity_from_record(
    record: &AgentTaskRunRecord,
) -> Option<homeboy_core::lab_contract::RunnerJobIdentity> {
    let handoff = record.lab_handoff.as_ref().filter(|handoff| {
        handoff.validation_error().is_none()
            && handoff.state == AgentTaskLabHandoffState::Accepted
            && handoff.authority == AgentTaskLabHandoffAuthority::RunnerDaemon
    })?;
    let identity = homeboy_core::lab_contract::RunnerJobIdentity::new(
        &record.run_id,
        &handoff.runner_id,
        handoff.runner_job_id.clone().unwrap_or_default(),
    );
    identity.is_complete().then_some(identity)
}

/// Metadata `kind` marker for a generic runner-execution run. It distinguishes
/// an ad hoc `runner exec --run-id` durable run from an agent-task lifecycle
/// record so ownership collisions are detectable (#8447).
pub const RUNNER_EXEC_RUN_KIND: &str = "runner_exec";

const RUNNER_EXEC_OBSERVATION_KIND: &str = "runner_execution";
const COMMAND_RESULT_SCHEMA: &str = "homeboy/command-result/v3";
const COMMAND_RESULT_STDOUT_LIMIT_BYTES: usize = 64 * 1024;
const COMMAND_RESULT_RUN_REF_LIMIT: usize = 32;
const DESCENDANT_RUN_GRAPH_LIMIT: usize = 64;

fn ensure_runner_exec_observation_run(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    runner_id: &str,
    remote_workspace: &str,
    remote_command: &[String],
    runner_job_id: Option<&str>,
) -> Result<homeboy_core::observation::RunRecord> {
    let run_id = sanitize_run_id(run_id);
    let store = lifecycle_store.open_observation_maintained()?;
    let mut run = match store.get_run(&run_id)? {
        Some(run)
            if run.metadata_json.get("kind").and_then(Value::as_str)
                == Some(RUNNER_EXEC_RUN_KIND)
                || run.kind == "runner-exec" =>
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
            command: Some(redact_argv(remote_command).join(" ")),
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
    metadata.insert(
        "remote_command".to_string(),
        json!(redact_argv(remote_command)),
    );
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
        &AgentTaskLifecycleStore::from_current_environment()?,
        run_id,
        runner_id,
        remote_workspace,
        remote_command,
        Some(runner_job_id),
    )
}

/// Persist controller and runner binary provenance independently of terminal
/// status. This is recorded before dispatch as well as after a runner response,
/// so pre-spawn failures retain the identities known at submission time.
pub fn record_runner_exec_execution_record(
    run_id: &str,
    execution_record: &homeboy_core::runner_execution_envelope::RunnerExecutionRecord,
) -> Result<()> {
    record_runner_exec_execution_record_in_store(
        &AgentTaskLifecycleStore::from_current_environment()?,
        run_id,
        execution_record,
    )
}

/// The store-rooted counterpart of [`record_runner_exec_execution_record`].
///
/// This is a read-modify-write of one observation row: read, `kind`-checked,
/// mutated, written back. Read and write are the same row, so they have to
/// be the same installation (#7505).
///
/// Opened through [`AgentTaskLifecycleStore::open_observation_maintained`]
/// rather than the lifecycle opener: the ambient body used
/// `ObservationStore::open_initialized()`, and the two differ in startup
/// artifact maintenance.
pub fn record_runner_exec_execution_record_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    execution_record: &homeboy_core::runner_execution_envelope::RunnerExecutionRecord,
) -> Result<()> {
    let store = lifecycle_store.open_observation_maintained()?;
    let Some(mut run) = store.get_run(&sanitize_run_id(run_id))? else {
        return Ok(());
    };
    if run.metadata_json.get("kind").and_then(Value::as_str) != Some(RUNNER_EXEC_RUN_KIND) {
        return Ok(());
    }
    run.metadata_json
        .as_object_mut()
        .expect("metadata object")
        .insert(
            "runner_execution_record".to_string(),
            serde_json::to_value(execution_record).expect("runner execution record serializes"),
        );
    store.upsert_imported_run_preserving_terminal(&run)
}

/// Persist extension-owned invocation identity and the terminal structured
/// result beside the generic runner execution record.
pub fn record_runner_exec_provider_result(
    run_id: &str,
    provider_id: &str,
    provider_version: &str,
    invocation: &[String],
    source_snapshot: &serde_json::Value,
    terminal_result: &serde_json::Value,
) -> Result<()> {
    let store = homeboy_core::observation::ObservationStore::open_initialized()?;
    let Some(mut run) = store.get_run(&sanitize_run_id(run_id))? else {
        return Ok(());
    };
    if run.metadata_json.get("kind").and_then(Value::as_str) != Some(RUNNER_EXEC_RUN_KIND) {
        return Ok(());
    }
    let metadata = run.metadata_json.as_object_mut().expect("metadata object");
    metadata.insert(
        "execution_provider".to_string(),
        json!({
            "id": provider_id,
            "version": provider_version,
            "invocation": invocation,
        }),
    );
    metadata.insert("source_snapshot".to_string(), source_snapshot.clone());
    metadata.insert("terminal_json_result".to_string(), terminal_result.clone());
    store.upsert_imported_run_preserving_terminal(&run)
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
    ensure_generic_runner_exec_run_in_store(
        &AgentTaskLifecycleStore::from_current_environment()?,
        run_id,
        runner_id,
        remote_workspace,
        remote_command,
    )
}

/// The store-rooted counterpart of [`ensure_generic_runner_exec_run`].
///
/// This is the run's first durable write. Everything the caller records after
/// it — declarations, execution record, promotions, terminal result — addresses
/// the row created here, so creating it in one installation and appending to it
/// from another would leave the later writes silently addressing nothing
/// (#7505).
pub fn ensure_generic_runner_exec_run_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    runner_id: &str,
    remote_workspace: &str,
    remote_command: &[String],
) -> Result<homeboy_core::observation::RunRecord> {
    ensure_runner_exec_observation_run(
        lifecycle_store,
        run_id,
        runner_id,
        remote_workspace,
        remote_command,
        None,
    )
}

/// Record a controller-side phase before work can reach a runner. This local
/// evidence never implies that a runner job exists.
pub fn record_runner_exec_pre_handoff_phase(run_id: &str, phase: &str) -> Result<()> {
    let store = homeboy_core::observation::ObservationStore::open_initialized()?;
    let Some(mut run) = store.get_run(&sanitize_run_id(run_id))? else {
        return Ok(());
    };
    if run.metadata_json.get("kind").and_then(Value::as_str) != Some(RUNNER_EXEC_RUN_KIND)
        || RunStatus::from_label(&run.status).is_some_and(RunStatus::is_terminal)
    {
        return Ok(());
    }
    run.metadata_json
        .as_object_mut()
        .expect("metadata object")
        .insert("runner_exec_phase".to_string(), json!(phase));
    store.upsert_imported_run_preserving_terminal(&run)
}

/// Terminalize an attempt that failed before a runner accepted a job. Once a
/// job is bound, normal runner reconciliation owns the terminal outcome.
pub fn finish_runner_exec_pre_handoff_failure(
    run_id: &str,
    transport: &str,
    phase: &str,
    handoff_accepted: bool,
    error: &Error,
) -> Result<bool> {
    let store = homeboy_core::observation::ObservationStore::open_initialized()?;
    let Some(run) = store.get_run(&sanitize_run_id(run_id))? else {
        return Ok(false);
    };
    if run.metadata_json.get("kind").and_then(Value::as_str) != Some(RUNNER_EXEC_RUN_KIND)
        || RunStatus::from_label(&run.status).is_some_and(RunStatus::is_terminal)
        || run.metadata_json.get("runner_job_id").is_some()
        || handoff_accepted
    {
        return Ok(false);
    }
    let recorded_phase = run.metadata_json["runner_exec_phase"]
        .as_str()
        .unwrap_or(phase)
        .to_string();
    let runner_id = run.metadata_json["runner_id"]
        .as_str()
        .unwrap_or("unknown")
        .to_string();
    let mut execution_record = run
        .metadata_json
        .get("runner_execution_record")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_else(|| {
            homeboy_core::runner_execution_envelope::RunnerExecutionRecord::terminal(
                run.id.clone(),
                &runner_id,
                transport,
                1,
            )
        });
    execution_record.status = "failed".to_string();
    store.finish_running_runner_exec_pre_handoff_failure(
        &run.id,
        json!({
            "phase": recorded_phase,
            "code": error.code.as_str(),
            "message": homeboy_core::redaction::redact_string(&error.message),
            "details": homeboy_core::redaction::redact_json(&error.details),
            "recovery": {
                "evidence": format!("homeboy runs evidence {run_id}"),
                "status": format!("homeboy runs show {run_id}"),
                "retry": format!("homeboy runner exec {runner_id} --run-id <new-run-id> -- <command>"),
            },
        }),
        json!({ "state": "pre_handoff_failed", "artifact_promotion": "not_started" }),
        serde_json::to_value(execution_record).expect("runner execution record serializes"),
    )
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
    record_runner_exec_artifact_declarations_in_store(
        &AgentTaskLifecycleStore::from_current_environment()?,
        run_id,
        artifacts,
        artifact_dirs,
        summaries,
    )
}

/// The store-rooted counterpart of [`record_runner_exec_artifact_declarations`].
///
/// This is a read-modify-write of one observation row: read, `kind`-checked,
/// mutated, written back. Read and write are the same row, so they have to
/// be the same installation (#7505).
///
/// Opened through [`AgentTaskLifecycleStore::open_observation_maintained`]
/// rather than the lifecycle opener: the ambient body used
/// `ObservationStore::open_initialized()`, and the two differ in startup
/// artifact maintenance.
pub fn record_runner_exec_artifact_declarations_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    artifacts: &[String],
    artifact_dirs: &[String],
    summaries: &[String],
) -> Result<()> {
    let store = lifecycle_store.open_observation_maintained()?;
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
    record_runner_exec_terminal_checkpoint_in_store(
        &AgentTaskLifecycleStore::from_current_environment()?,
        run_id,
        snapshot,
    )
}

/// The store-rooted counterpart of [`record_runner_exec_terminal_checkpoint`].
///
/// This is a read-modify-write of one observation row: the row is read,
/// its `kind` is checked, its metadata is mutated, and it is written back.
/// Read and write are the same row, so they have to be the same
/// installation (#7505).
///
/// Opened through [`AgentTaskLifecycleStore::open_observation_maintained`]
/// rather than the lifecycle opener, because the ambient body used
/// `ObservationStore::open_initialized()` and the two differ in startup
/// artifact maintenance. Rooting through the lifecycle opener would change
/// behaviour, not just its home.
pub fn record_runner_exec_terminal_checkpoint_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    snapshot: &RunnerJobLogSnapshot,
) -> Result<()> {
    if !snapshot.job.status.is_terminal() {
        return Ok(());
    }
    let store = lifecycle_store.open_observation_maintained()?;
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

/// Preserve the authoritative terminal snapshot when controller-side evidence
/// projection fails. The run stays active for replay, while its job reference
/// and bounded daemon events remain available to an operator.
pub fn record_runner_exec_projection_failure(
    run_id: &str,
    snapshot: &RunnerJobLogSnapshot,
    error: &Error,
) -> Result<()> {
    record_runner_exec_terminal_checkpoint(run_id, snapshot)?;
    let store = homeboy_core::observation::ObservationStore::open_initialized()?;
    let Some(mut run) = store.get_run(&sanitize_run_id(run_id))? else {
        return Ok(());
    };
    if run.metadata_json.get("kind").and_then(Value::as_str) != Some(RUNNER_EXEC_RUN_KIND) {
        return Ok(());
    }
    let metadata = run.metadata_json.as_object_mut().expect("metadata object");
    metadata.insert(
        "runner_terminal_projection".to_string(),
        json!({
            "state": "projection_failed",
            "artifact_promotion": "pending",
            "job_id": snapshot.job.id,
            "status": snapshot.job.status,
            "event_count": snapshot.events.len(),
            "error": { "code": error.code.as_str(), "message": error.message },
        }),
    );
    store.upsert_imported_run_preserving_terminal(&run)
}

/// This is a read-modify-write of one observation row: the row is read,
/// its `kind` is checked, its metadata is mutated, and it is written back.
/// Read and write are the same row, so they have to be the same
/// installation (#7505).
///
/// Opened through [`AgentTaskLifecycleStore::open_observation_maintained`]
/// rather than the lifecycle opener, because the ambient body used
/// `ObservationStore::open_initialized()` and the two differ in startup
/// artifact maintenance. Rooting through the lifecycle opener would change
/// behaviour, not just its home.
pub fn record_runner_exec_artifact_refs_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    artifacts: &[homeboy_core::observation::ArtifactRecord],
) -> Result<()> {
    let store = lifecycle_store.open_observation_maintained()?;
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

/// This is a read-modify-write of one observation row: the row is read,
/// its `kind` is checked, its metadata is mutated, and it is written back.
/// Read and write are the same row, so they have to be the same
/// installation (#7505).
///
/// Opened through [`AgentTaskLifecycleStore::open_observation_maintained`]
/// rather than the lifecycle opener, because the ambient body used
/// `ObservationStore::open_initialized()` and the two differ in startup
/// artifact maintenance. Rooting through the lifecycle opener would change
/// behaviour, not just its home.
pub fn record_runner_exec_declaration_promotion_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    role: &str,
    declaration: &str,
    artifacts: &[homeboy_core::observation::ArtifactRecord],
) -> Result<()> {
    let store = lifecycle_store.open_observation_maintained()?;
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
        .get("runner_exec_declaration_promotions")
        .and_then(Value::as_object)
        .and_then(|promotions| promotions.get(&format!("{role}:{declaration}")))
        .and_then(|promotion| promotion.get("state"))
        .and_then(Value::as_str)
        == Some("promoted")
}

/// Bind a directory declaration to one immutable recursive tree before any of
/// its children are copied. A replay of changed runner-side output fails closed
/// instead of mixing two directory versions under one declaration.
pub fn checkpoint_runner_exec_directory_tree(
    run_id: &str,
    declaration: &str,
    tree_sha256: &str,
) -> Result<()> {
    checkpoint_runner_exec_directory_tree_in_store(
        &AgentTaskLifecycleStore::from_current_environment()?,
        run_id,
        declaration,
        tree_sha256,
    )
}

/// The store-rooted counterpart of [`checkpoint_runner_exec_directory_tree`].
///
/// The directory checkpoint and the per-child promotion states it guards live
/// in the same metadata object on the same row, so they must be the same
/// installation (#7505).
pub fn checkpoint_runner_exec_directory_tree_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    declaration: &str,
    tree_sha256: &str,
) -> Result<()> {
    let store = lifecycle_store.open_observation_maintained()?;
    let Some(mut run) = store.get_run(&sanitize_run_id(run_id))? else {
        return Ok(());
    };
    if run.metadata_json.get("kind").and_then(Value::as_str) != Some(RUNNER_EXEC_RUN_KIND) {
        return Ok(());
    }
    let metadata = run.metadata_json.as_object_mut().expect("metadata object");
    let mut checkpoints = metadata
        .get("runner_exec_directory_promotions")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Some(existing) = checkpoints
        .get(declaration)
        .and_then(|value| value.get("tree_sha256"))
        .and_then(Value::as_str)
        .filter(|existing| *existing != tree_sha256)
    {
        return Err(Error::validation_invalid_argument(
            "artifact_dir",
            format!(
                "runner exec artifact directory changed after promotion checkpoint ({existing})"
            ),
            Some(declaration.to_string()),
            None,
        ));
    }
    checkpoints
        .entry(declaration.to_string())
        .or_insert_with(|| json!({ "tree_sha256": tree_sha256, "children": {} }));
    metadata.insert(
        "runner_exec_directory_promotions".to_string(),
        Value::Object(checkpoints),
    );
    store.upsert_imported_run_preserving_terminal(&run)
}

/// This is the idempotence guard for directory-child promotion: it decides
/// whether a child is skipped or promoted again. Reading it from a different
/// installation than the one the promotion is written to makes the guard
/// answer about a row nobody is writing (#7505).
pub fn runner_exec_directory_child_is_promoted_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    declaration: &str,
    relative_child: &str,
) -> Result<bool> {
    let store = lifecycle_store.open_observation_maintained()?;
    let Some(run) = store.get_run(&sanitize_run_id(run_id))? else {
        return Ok(false);
    };
    Ok(run
        .metadata_json
        .get("runner_exec_directory_promotions")
        .and_then(|value| value.get(declaration))
        .and_then(|value| value.get("children"))
        .and_then(|value| value.get(relative_child))
        .and_then(|value| value.get("state"))
        .and_then(Value::as_str)
        == Some("promoted"))
}

pub fn record_runner_exec_directory_child_promotion(
    run_id: &str,
    declaration: &str,
    relative_child: &str,
    artifact: &homeboy_core::observation::ArtifactRecord,
) -> Result<()> {
    record_runner_exec_directory_child_promotion_in_store(
        &AgentTaskLifecycleStore::from_current_environment()?,
        run_id,
        declaration,
        relative_child,
        artifact,
    )
}

/// The store-rooted counterpart of [`record_runner_exec_directory_child_promotion`].
///
/// The write half of the guard in `runner_exec_directory_child_is_promoted`.
/// Guard and write are the same metadata object, so they share a store (#7505).
pub fn record_runner_exec_directory_child_promotion_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    declaration: &str,
    relative_child: &str,
    artifact: &homeboy_core::observation::ArtifactRecord,
) -> Result<()> {
    let store = lifecycle_store.open_observation_maintained()?;
    let Some(mut run) = store.get_run(&sanitize_run_id(run_id))? else {
        return Ok(());
    };
    let metadata = run.metadata_json.as_object_mut().expect("metadata object");
    let mut checkpoints = metadata
        .get("runner_exec_directory_promotions")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let checkpoint = checkpoints
        .get_mut(declaration)
        .and_then(Value::as_object_mut)
        .ok_or_else(|| {
            Error::internal_unexpected("runner-exec directory child has no tree checkpoint")
        })?;
    let children = checkpoint
        .entry("children")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .expect("directory children object");
    children.insert(
        relative_child.to_string(),
        json!({
            "state": "promoted",
            "id": artifact.id,
            "sha256": artifact.sha256,
            "artifact_type": artifact.artifact_type,
        }),
    );
    metadata.insert(
        "runner_exec_directory_promotions".to_string(),
        Value::Object(checkpoints),
    );
    store.upsert_imported_run_preserving_terminal(&run)
}

/// This finalizes an observation run: it reads the row, decides from that row's
/// own `kind`, terminal status, and artifact-promotion checkpoint whether to
/// project at all, and then commits the result with `finish_run`. The decision
/// and the write are the same row, so they have to be the same database — a
/// projection that read one installation's row and finished another's would
/// either refuse a valid terminal result or overwrite an unrelated run.
///
/// The observation store is opened through
/// [`AgentTaskLifecycleStore::open_observation_maintained`], not the lifecycle
/// opener: the ambient body used `ObservationStore::open_initialized()`, which
/// runs startup artifact maintenance, and this path gates on a durable
/// artifact-promotion checkpoint. Rooting it through the maintenance-deferring
/// lifecycle opener would have changed behaviour rather than just its home
/// (#7505).
pub fn project_terminal_runner_exec_result_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    snapshot: &RunnerJobLogSnapshot,
) -> Result<bool> {
    if !snapshot.job.status.is_terminal() {
        return Ok(false);
    }
    let store = lifecycle_store.open_observation_maintained()?;
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
    if let Some(descendants) = terminal_command_result_descendants(&store, &run.id, snapshot) {
        metadata.insert(
            "descendant_run_evidence".to_string(),
            serde_json::to_value(descendants).expect("descendant refs serialize"),
        );
    }
    metadata.insert("runner_job_id".to_string(), json!(snapshot.job.id));
    metadata.insert("runner_job_status".to_string(), json!(snapshot.job.status));
    metadata.insert("runner_job_events".to_string(), json!(snapshot.events));
    let mut execution_record = metadata
        .get("runner_execution_record")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_else(|| {
            homeboy_core::runner_execution_envelope::RunnerExecutionRecord::terminal(
                snapshot.job.id.to_string(),
                runner_id.clone(),
                "daemon",
                exit_code,
            )
            .with_job_id(snapshot.job.id.to_string())
        });
    execution_record.status = if snapshot.job.status == JobStatus::Succeeded {
        "succeeded".to_string()
    } else {
        "failed".to_string()
    };
    execution_record.job_id = Some(snapshot.job.id.to_string());
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

/// Accept only a complete, bounded command-result envelope whose run refs point
/// to locally persisted child observations. A terminal log is runner-controlled,
/// so any malformed field, truncation marker, or unresolved reference rejects
/// the whole projection rather than preserving a plausible false edge.
pub(crate) fn terminal_command_result_descendants(
    store: &homeboy_core::observation::ObservationStore,
    parent_run_id: &str,
    snapshot: &RunnerJobLogSnapshot,
) -> Option<Vec<homeboy_core::observation::evidence_report::DescendantRunEvidenceRef>> {
    let result = snapshot
        .events
        .iter()
        .rev()
        .find(|event| matches!(event.kind, homeboy_core::api_jobs::JobEventKind::Result))?
        .data
        .as_ref()?;
    if result
        .pointer("/capture/stdout/truncated")
        .and_then(Value::as_bool)
        == Some(true)
    {
        return None;
    }
    let stdout = result.get("stdout")?.as_str()?;
    if stdout.len() > COMMAND_RESULT_STDOUT_LIMIT_BYTES {
        return None;
    }
    let envelope: Value = serde_json::from_str(stdout).ok()?;
    if envelope.get("schema").and_then(Value::as_str) != Some(COMMAND_RESULT_SCHEMA)
        || !bounded_string(&envelope, "command", 128)
        || envelope.get("success").and_then(Value::as_bool).is_none()
        || envelope
            .get("exit_code")
            .and_then(Value::as_i64)
            .and_then(|code| i32::try_from(code).ok())
            .is_none()
        || !bounded_string(&envelope, "status", 64)
    {
        return None;
    }
    let runs = envelope.pointer("/refs/runs")?.as_array()?;
    if runs.len() > COMMAND_RESULT_RUN_REF_LIMIT {
        return None;
    }
    let mut descendants = Vec::with_capacity(runs.len());
    for run in runs {
        let id = bounded_required_string(run, "id", 256)?;
        let kind = bounded_required_string(run, "kind", 128)?;
        let _source = bounded_required_string(run, "source", 256)?;
        if id == parent_run_id {
            return None;
        }
        let child = store.get_run(id).ok().flatten()?;
        if kind != child.kind {
            return None;
        }
        if descendant_run_reaches(store, &child.id, parent_run_id)? {
            return None;
        }
        let reference = homeboy_core::observation::evidence_report::DescendantRunEvidenceRef {
            schema: homeboy_core::observation::evidence_report::DESCENDANT_RUN_EVIDENCE_REF_SCHEMA
                .to_string(),
            run_id: child.id,
            kind: child.kind,
            source: homeboy_core::observation::evidence_report::DESCENDANT_RUN_EVIDENCE_SOURCE_TERMINAL_COMMAND_RESULT.to_string(),
        };
        if !reference.is_valid() || descendants.iter().any(|existing: &homeboy_core::observation::evidence_report::DescendantRunEvidenceRef| existing.run_id == reference.run_id) {
            return None;
        }
        descendants.push(reference);
    }
    Some(descendants)
}

/// Follow only persisted, typed descendant edges. Every hop is revalidated
/// against the current observation record and the traversal fails closed once
/// its bounded budget is exhausted.
fn descendant_run_reaches(
    store: &homeboy_core::observation::ObservationStore,
    start_run_id: &str,
    target_run_id: &str,
) -> Option<bool> {
    let mut pending = vec![start_run_id.to_string()];
    let mut visited = std::collections::HashSet::new();
    while let Some(run_id) = pending.pop() {
        if run_id == target_run_id {
            return Some(true);
        }
        if !visited.insert(run_id.clone()) {
            continue;
        }
        if visited.len() > DESCENDANT_RUN_GRAPH_LIMIT {
            return None;
        }
        let run = store.get_run(&run_id).ok().flatten()?;
        let Some(value) = run.metadata_json.get("descendant_run_evidence") else {
            continue;
        };
        let refs = serde_json::from_value::<
            Vec<homeboy_core::observation::evidence_report::DescendantRunEvidenceRef>,
        >(value.clone())
        .ok()?;
        if refs.len() > COMMAND_RESULT_RUN_REF_LIMIT {
            return None;
        }
        for reference in refs {
            if !reference.is_valid() {
                return None;
            }
            let child = store.get_run(&reference.run_id).ok().flatten()?;
            if child.kind != reference.kind {
                return None;
            }
            pending.push(child.id);
        }
    }
    Some(false)
}

fn bounded_string(value: &Value, field: &str, limit: usize) -> bool {
    bounded_required_string(value, field, limit).is_some()
}

fn bounded_required_string<'a>(value: &'a Value, field: &str, limit: usize) -> Option<&'a str> {
    let value = value.get(field)?.as_str()?;
    (!value.is_empty() && value.len() <= limit && !value.chars().any(char::is_control))
        .then_some(value)
}

/// This is the run's terminal commit. It reads the row, refuses if already
/// terminal, and finishes it — the guard and the write are the same row, so a
/// split root could finish a run the guard never inspected (#7505).
pub fn finish_runner_exec_direct_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    transport: &str,
    exit_code: i32,
) -> Result<bool> {
    let store = lifecycle_store.open_observation_maintained()?;
    let Some(mut run) = store.get_run(&sanitize_run_id(run_id))? else {
        return Ok(false);
    };
    if run.metadata_json.get("kind").and_then(Value::as_str) != Some(RUNNER_EXEC_RUN_KIND)
        || RunStatus::from_label(&run.status).is_some_and(RunStatus::is_terminal)
    {
        return Ok(false);
    }
    let runner_id = run.metadata_json["runner_id"]
        .as_str()
        .unwrap_or("unknown")
        .to_string();
    let metadata = run.metadata_json.as_object_mut().expect("metadata object");
    let mut execution_record = metadata
        .get("runner_execution_record")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_else(|| {
            homeboy_core::runner_execution_envelope::RunnerExecutionRecord::terminal(
                run.id.clone(),
                runner_id,
                transport,
                exit_code,
            )
        });
    execution_record.status = if exit_code == 0 {
        "succeeded".to_string()
    } else {
        "failed".to_string()
    };
    metadata.insert(
        "runner_execution_record".to_string(),
        serde_json::to_value(execution_record).expect("runner execution record serializes"),
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
    let snapshot_runner_id = snapshot
        .job
        .target_runner_id
        .as_deref()
        .or_else(|| {
            snapshot
                .job
                .runner_job_projection
                .as_ref()
                .map(|projection| projection.runner_id.as_str())
        })
        .unwrap_or_default();
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

/// Record the submission intent against an explicitly injected root.
///
/// The body already took one store for the advisory lock and every record touch
/// under it; this lets a caller that owns roots supply that store instead of
/// having a second one resolved behind it (#7505).
pub fn record_lab_offload_submission_intent_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    runner_id: &str,
    remote_workspace: &str,
    remote_command: &[String],
    secret_env_names: &[String],
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let _lock = LabHandoffLock::lock_in_store(lifecycle_store, &run_id)?;
    let mut record = lifecycle_store.read_record(&run_id)?;
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
    lifecycle_store.write_record(&record)?;
    Ok(record)
}

/// Replace a preflight intent with the exact normalized, redacted request that
/// will cross the broker boundary. This is the final durable write before POST.
#[cfg(test)]
pub fn record_lab_offload_submission_request(
    run_id: &str,
    request: &RemoteRunnerJobRequest,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    // As above: the lock and the record write it guards resolve one root once.
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    let _lock = LabHandoffLock::lock_in_store(&lifecycle_store, &run_id)?;
    let mut record = lifecycle_store.read_record(&run_id)?;
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
    lifecycle_store.write_record(&record)?;
    Ok(record)
}

/// Persist the envelope-first replay input before it crosses the broker.
pub fn record_lab_offload_submission_envelope(
    run_id: &str,
    request: &homeboy_runner_contract::RunnerApiSubmitRequest,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let dispatch = request.envelope.dispatch.as_ref().ok_or_else(|| {
        Error::internal_unexpected("Lab runner submission envelope has no dispatch")
    })?;
    if request.submission_key.trim().is_empty() {
        return Err(Error::internal_unexpected(
            "Lab runner submission envelope has no stable submission key",
        ));
    }
    let payload_fingerprint =
        homeboy_core::api_jobs::runner_api_submission_payload_fingerprint(request)?;
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    let _lock = LabHandoffLock::lock_in_store(&lifecycle_store, &run_id)?;
    let mut record = lifecycle_store.read_record(&run_id)?;
    if record.state.is_terminal() {
        return Ok(record);
    }
    let now = chrono::Utc::now();
    let mut handoff = AgentTaskLabHandoff::pending(
        &dispatch.runner_id,
        now.to_rfc3339(),
        (now + chrono::Duration::seconds(lab_handoff_acceptance_timeout_seconds())).to_rfc3339(),
    );
    handoff.submission_key = Some(request.submission_key.clone());
    handoff.payload_fingerprint = Some(payload_fingerprint.clone());
    record.lab_handoff = Some(handoff.clone());
    record.ensure_metadata_object().insert(
        "runner_submission_intent".to_string(),
        json!({
            "state": "pending",
            "submission_key": request.submission_key,
            "payload_fingerprint": payload_fingerprint,
            "runner_id": dispatch.runner_id,
            "replay_envelope_request": request,
        }),
    );
    record.ensure_metadata_object().insert(
        "handoff_acceptance".to_string(),
        json!({
            "state": "pending",
            "started_at": handoff.submitted_at,
            "deadline_at": handoff.acceptance_deadline_at,
        }),
    );
    lifecycle_store.write_record(&record)?;
    Ok(record)
}
