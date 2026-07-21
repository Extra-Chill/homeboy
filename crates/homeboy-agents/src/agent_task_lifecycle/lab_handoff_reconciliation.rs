//! Lab-runner handoff and pending-submission-intent reconciliation.
//!
//! The volatile heart of the durable-run control plane: reconciling active lab
//! handoffs, binding/expiring pending runner-submission intents, terminalizing
//! lost accepted lab jobs, and the handoff/candidate follow-up predicates.
//! Extracted from the high-churn `lifecycle_ops` god file to isolate this
//! reconciliation state machine into a named, testable seam.

use super::*;
use homeboy_core::api_jobs::RemoteRunnerJobRequest;

pub fn reconcile_active_lab_runner_handoffs() -> Result<usize> {
    let now = chrono::Utc::now();
    let mut accepted_run_ids = Vec::new();
    let mut pending_intent_run_ids = Vec::new();
    let mut expired_run_ids = Vec::new();
    // Scan the stored snapshot so this operation owns and reports its expiry
    // mutations. `list_records` refreshes through `status`, which also expires
    // pending handoffs as a user-visible read-side convergence guarantee.
    for record in store::read_records()? {
        if record.state == AgentTaskRunState::Running && record.has_accepted_lab_handoff() {
            accepted_run_ids.push(record.run_id);
        } else if has_pending_runner_submission_intent(&record) {
            pending_intent_run_ids.push(record.run_id);
        } else if has_expired_pending_runner_submission_intent(&record, now) {
            expired_run_ids.push(record.run_id);
        }
    }

    let mut reconciled = 0;
    for run_id in pending_intent_run_ids {
        if reconcile_pending_runner_submission_intent(&run_id)? {
            reconciled += 1;
        }
    }
    for run_id in expired_run_ids {
        if expire_unaccepted_lab_handoff(&run_id)? {
            reconciled += 1;
        }
    }
    for run_id in accepted_run_ids {
        // `status` owns snapshot validation, persistence, and the exact
        // no-PID daemon-loss projection. A bad remote record must not prevent
        // unrelated activity from being listed.
        if status(&run_id).is_ok() {
            reconciled += 1;
        }
    }
    Ok(reconciled)
}

fn has_pending_runner_submission_intent(record: &AgentTaskRunRecord) -> bool {
    has_complete_pending_runner_submission_intent(record)
        && record
            .lab_handoff
            .as_ref()
            .and_then(|handoff| handoff.acceptance_deadline_at.as_deref())
            .and_then(|timestamp| chrono::DateTime::parse_from_rfc3339(timestamp).ok())
            .is_some_and(|deadline| deadline.with_timezone(&chrono::Utc) > chrono::Utc::now())
}

pub(crate) fn has_expired_pending_runner_submission_intent(
    record: &AgentTaskRunRecord,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    record.state == AgentTaskRunState::Queued
        && record.runner_job_id().is_none()
        && record.lab_handoff.as_ref().is_some_and(|handoff| {
            handoff.state == AgentTaskLabHandoffState::Pending
                && handoff.authority == AgentTaskLabHandoffAuthority::Controller
        })
        && record.has_expired_pending_lab_handoff(now)
}

fn has_complete_pending_runner_submission_intent(record: &AgentTaskRunRecord) -> bool {
    if record.state != AgentTaskRunState::Queued || record.runner_job_id().is_some() {
        return false;
    }
    let Some(handoff) = record.lab_handoff.as_ref().filter(|handoff| {
        handoff.state == AgentTaskLabHandoffState::Pending
            && handoff.authority == AgentTaskLabHandoffAuthority::Controller
    }) else {
        return false;
    };
    let Some(intent) = record
        .metadata
        .get("runner_submission_intent")
        .and_then(Value::as_object)
        .filter(|intent| intent.get("state").and_then(Value::as_str) == Some("pending"))
    else {
        return false;
    };
    let Some(submission_key) = intent
        .get("submission_key")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    else {
        return false;
    };
    let Some(runner_id) = intent
        .get("runner_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    else {
        return false;
    };
    let Ok(request) = serde_json::from_value::<RemoteRunnerJobRequest>(
        intent.get("replay_request").cloned().unwrap_or(Value::Null),
    ) else {
        return false;
    };
    request.runner_id == runner_id
        && request.submission_key() == Some(submission_key)
        && handoff.runner_id == runner_id
        && handoff.submission_key.as_deref() == Some(submission_key)
}

/// Replay an unacknowledged durable handoff. A broker that already accepted the
/// original POST returns the same job for its submission key, so this covers
/// both controller crash boundaries without retaining secret values.
pub fn reconcile_pending_runner_submission_intent(run_id: &str) -> Result<bool> {
    let run_id = sanitize_run_id(run_id);
    let record = store::read_record(&run_id)?;
    if !has_pending_runner_submission_intent(&record) {
        return Ok(false);
    }
    let intent = record
        .metadata
        .get("runner_submission_intent")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            Error::internal_unexpected("pending runner submission intent is malformed")
        })?;
    let string = |key: &str| {
        intent
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
    };
    let runner_id = string("runner_id").ok_or_else(|| {
        Error::internal_unexpected("pending runner submission intent has no runner id")
    })?;
    let submission_key = string("submission_key").ok_or_else(|| {
        Error::internal_unexpected("pending runner submission intent has no submission key")
    })?;
    let mut request: RemoteRunnerJobRequest =
        serde_json::from_value(intent.get("replay_request").cloned().ok_or_else(|| {
            Error::internal_unexpected(
                "pending runner submission intent has no complete replay request",
            )
        })?)
        .map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("parse runner replay request".to_string()),
            )
        })?;
    if request.runner_id != runner_id {
        return Err(Error::internal_unexpected(
            "runner replay request does not match pending runner",
        ));
    }
    let cwd = request.cwd.clone().unwrap_or_default();
    let command = request.command.clone();
    let mut metadata = request.metadata.take().unwrap_or_else(|| json!({}));
    if !metadata.is_object() {
        metadata = json!({});
    }
    metadata["submission_key"] = json!(submission_key);
    metadata["durable_run_id"] = json!(run_id);
    metadata["reconciled_from"] = json!("durable_detached_handoff_intent");
    request.metadata = Some(metadata);
    match runner_continuation::with_runner_continuation(|provider| {
        provider.submit_reverse_broker_job(&runner_id, request)
    }) {
        Ok(job) => {
            record_detached_lab_run(DetachedLabRunRecord {
                run_id: &run_id,
                runner_id: &runner_id,
                runner_job_id: &job.id.to_string(),
                remote_workspace: &cwd,
                remote_command: &command,
            })?;
            Ok(true)
        }
        Err(error) => {
            let _ = store::mutate_record(&run_id, |record| {
                let metadata = record.ensure_metadata_object();
                metadata["runner_submission_intent"]["last_reconciliation_error"] = json!({
                    "code": error.code.as_str(),
                    "message": error.message,
                    "retryable": true,
                });
                true
            })?;
            Ok(false)
        }
    }
}

/// Resolve a possibly accepted submission without replaying it. This is used
/// only after the acceptance deadline or an operator cancellation request:
/// those paths must never create new runner work.
pub(crate) fn bind_pending_runner_submission_if_accepted(run_id: &str) -> Result<bool> {
    let run_id = sanitize_run_id(run_id);
    let record = store::read_record(&run_id)?;
    if record.runner_job_id().is_some()
        || record
            .metadata
            .pointer("/runner_submission_intent/state")
            .and_then(Value::as_str)
            != Some("pending")
    {
        return Ok(false);
    }
    let intent = record
        .metadata
        .get("runner_submission_intent")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            Error::internal_unexpected("pending runner submission intent is malformed")
        })?;
    let string = |key: &str| {
        intent
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
    };
    let runner_id = string("runner_id").ok_or_else(|| {
        Error::internal_unexpected("pending runner submission intent has no runner id")
    })?;
    let submission_key = string("submission_key").ok_or_else(|| {
        Error::internal_unexpected("pending runner submission intent has no submission key")
    })?;
    let request: RemoteRunnerJobRequest =
        serde_json::from_value(intent.get("replay_request").cloned().ok_or_else(|| {
            Error::internal_unexpected(
                "pending runner submission intent has no complete replay request",
            )
        })?)
        .map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("parse runner replay request".to_string()),
            )
        })?;
    if request.runner_id != runner_id {
        return Err(Error::internal_unexpected(
            "runner replay request does not match pending runner",
        ));
    }
    let lookup = runner_continuation::with_runner_continuation(|provider| {
        provider.lookup_reverse_broker_submission(&runner_id, &submission_key)
    });
    match lookup {
        Ok(homeboy_core::api_jobs::RemoteRunnerSubmissionLookup::Accepted { job }) => {
            record_detached_lab_run(DetachedLabRunRecord {
                run_id: &run_id,
                runner_id: &runner_id,
                runner_job_id: &job.id.to_string(),
                remote_workspace: request.cwd.as_deref().unwrap_or_default(),
                remote_command: &request.command,
            })?;
            Ok(true)
        }
        Ok(
            homeboy_core::api_jobs::RemoteRunnerSubmissionLookup::Absent
            | homeboy_core::api_jobs::RemoteRunnerSubmissionLookup::Expired { .. },
        ) => Ok(false),
        Err(error) => {
            let _ = store::mutate_record(&run_id, |record| {
                record.ensure_metadata_object()["runner_submission_intent"]["last_lookup_error"] = json!({
                    "code": error.code.as_str(),
                    "message": error.message.clone(),
                    "retryable": true,
                });
                true
            })?;
            Err(error)
        }
    }
}

/// Whether the controller has durably transferred this run to a specific
/// runner daemon job. Once this is true, controller transport errors are not
/// pre-acceptance failures: the stored runner identity is the reconciliation
/// boundary for the eventual authoritative result.
pub fn has_accepted_runner_handoff(run_id: &str) -> Result<bool> {
    let record = store::read_record(&sanitize_run_id(run_id))?;
    Ok(is_accepted_runner_handoff(&record))
}

/// Whether a run already carries durable provider-execution progress, so a
/// later transport/handoff error must preserve its candidate instead of
/// terminalizing it as a pre-execution failure (#9377). Returns `false` when no
/// record exists yet (a genuine pre-execution failure).
pub fn has_recorded_provider_progress(run_id: &str) -> Result<bool> {
    match store::read_record(&sanitize_run_id(run_id)) {
        Ok(record) => Ok(record.has_recorded_provider_progress()),
        Err(_) => Ok(false),
    }
}

/// Whether a run recorded a candidate-preserving post-provider transport
/// follow-up failure (#9377). The runner uses this to keep a run-scoped
/// workspace when a provider succeeded but its controller-side candidate
/// projection or handoff has not — so the preserved candidate remains
/// recoverable on the lab. Returns `false` when no record exists.
pub fn run_owes_candidate_follow_up(run_id: &str) -> Result<bool> {
    match store::read_record(&sanitize_run_id(run_id)) {
        Ok(record) => Ok(record.metadata.get("transport_follow_up_failure").is_some()
            || record
                .metadata
                .get("candidate_preserved")
                .and_then(Value::as_bool)
                == Some(true)),
        Err(_) => Ok(false),
    }
}

/// Pure durable-handoff predicate for callers that already hold the lifecycle
/// record and must not re-enter the store.
pub(crate) fn is_accepted_runner_handoff(record: &AgentTaskRunRecord) -> bool {
    record.has_accepted_lab_handoff()
}

/// Reconstruct an authenticated pre-provider transport failure that can safely
/// admit an externally prepared immutable candidate. Expired handoffs retain
/// their aggregate-free legacy shape; preacceptance failures retain their
/// canonical failure aggregate and its synthetic runtime projection.
pub(crate) fn expire_unaccepted_lab_handoff(run_id: &str) -> Result<bool> {
    // An expired pending request may have been accepted immediately before its
    // response was lost. Querying its key is read-only; never replay here.
    if bind_pending_runner_submission_if_accepted(run_id)? {
        return Ok(true);
    }
    let _lock = LabHandoffLock::lock(run_id)?;
    // Re-read while holding the handoff lock: an accepted job is runner-owned
    // and must never be terminalized by controller deadline recovery.
    let record = store::read_record(run_id)?;
    if !has_expired_pending_runner_submission_intent(&record, chrono::Utc::now()) {
        return Ok(false);
    }

    let mut record = cancel_run(run_id, Some(EXPIRED_LAB_HANDOFF_REASON))?;
    let expired_at = now_timestamp();
    let record_run_id = record.run_id.clone();
    let runner_id = record.runner_id().unwrap_or_default().to_string();
    if let Some(handoff) = record.lab_handoff.as_ref() {
        record.lab_handoff = Some(handoff.expired(expired_at.clone()));
    }
    let metadata = record.ensure_metadata_object();
    metadata.insert(
        "handoff_acceptance".to_string(),
        json!({
            "state": "expired",
            "expired_at": expired_at,
            "reason": EXPIRED_LAB_HANDOFF_REASON,
        }),
    );
    metadata.insert("phase".to_string(), json!("handoff_rejected"));
    metadata.insert("provider_executions_consumed".to_string(), json!(0));
    metadata.insert(
        "phase_activity".to_string(),
        json!("runner handoff acceptance deadline expired before runner acceptance"),
    );
    metadata.insert(METADATA_KEY_RETRYABLE.to_string(), json!(true));
    metadata.insert(
        "runner_execution_record".to_string(),
        serde_json::to_value(
            homeboy_core::runner_execution_envelope::RunnerExecutionRecord::terminal(
                &record_run_id,
                runner_id,
                "daemon",
                1,
            )
            .with_agent_task_run_id(record_run_id),
        )
        .unwrap_or(Value::Null),
    );
    store::write_record(&record)?;
    Ok(true)
}

pub(crate) fn reconcile_runner_job_state(record: &mut AgentTaskRunRecord) -> Result<()> {
    if record.state != AgentTaskRunState::Running {
        return Ok(());
    }
    let (Some(runner_id), Some(job_id)) = (
        record.runner_id().map(str::to_string),
        record.runner_job_id().map(str::to_string),
    ) else {
        return Ok(());
    };
    match super::runner_continuation::with_runner_continuation(|p| {
        p.reconcile_runner_job(&runner_id, &job_id)
    }) {
        super::runner_continuation::RunnerJobReconciliation::Snapshot(snapshot) => {
            reconcile_runner_job_snapshot(record, &snapshot)
        }
        super::runner_continuation::RunnerJobReconciliation::ConfirmedAbsent {
            checked_generations,
        } => terminalize_lost_accepted_lab_job(record, checked_generations),
        super::runner_continuation::RunnerJobReconciliation::UnconfirmedAbsence => {
            let disconnected = super::runner_continuation::with_runner_continuation(|p| {
                !p.is_runner_connected(&runner_id)
            });
            if disconnected {
                record.annotate_runner_disconnected();
            }
            Ok(())
        }
    }
}

fn terminalize_lost_accepted_lab_job(
    record: &mut AgentTaskRunRecord,
    checked_generations: usize,
) -> Result<()> {
    if !record.has_accepted_lab_handoff() || !record.provider_handles.is_empty() {
        return Ok(());
    }
    let plan = store::read_controller_plan(&record.run_id)?;
    let run_id = record.run_id.clone();
    let runner_id = record.runner_id().unwrap_or_default().to_string();
    let runner_job_id = record.runner_job_id().unwrap_or_default().to_string();
    let mut error = Error::internal_unexpected(format!(
        "accepted Lab runner job '{runner_job_id}' was not found in any of {checked_generations} authoritative known daemon generation(s)"
    ))
    .with_hint(format!("Retry safely: homeboy agent-task retry {run_id} --run"));
    error.retryable = Some(true);
    let mut terminal = crate::agent_task_lifecycle::record_pre_execution_failure(
        &run_id,
        &plan,
        "accepted_lab_runner_job_lost",
        &error,
    )?;
    let metadata = terminal.ensure_metadata_object();
    metadata.insert("phase".to_string(), json!("accepted_lab_runner_job_lost"));
    metadata.insert(
        "phase_activity".to_string(),
        json!("accepted runner job was confirmed absent across authoritative daemon generations"),
    );
    metadata.insert("provider_executions_consumed".to_string(), json!(0));
    metadata.insert(METADATA_KEY_RETRYABLE.to_string(), json!(true));
    metadata.insert(
        "lost_accepted_runner_job".to_string(),
        json!({
            "runner_id": runner_id,
            "runner_job_id": runner_job_id,
            "checked_generations": checked_generations,
            "safe_next_action": format!("homeboy agent-task retry {run_id} --run"),
        }),
    );
    if let Some(handoff) = metadata.get_mut("runner_handoff") {
        handoff["state"] = json!("lost");
    }
    store::write_record(&terminal)?;
    *record = terminal;
    Ok(())
}

#[cfg(test)]
pub(crate) fn apply_runner_job_terminal_state(
    record: &mut AgentTaskRunRecord,
    status: homeboy_core::api_jobs::JobStatus,
    events: &[homeboy_core::api_jobs::JobEvent],
) {
    let (run_state, task_state) = match status {
        homeboy_core::api_jobs::JobStatus::Succeeded => {
            (AgentTaskRunState::Succeeded, AgentTaskState::Succeeded)
        }
        homeboy_core::api_jobs::JobStatus::Cancelled => {
            (AgentTaskRunState::Cancelled, AgentTaskState::Cancelled)
        }
        homeboy_core::api_jobs::JobStatus::Failed => {
            (AgentTaskRunState::Failed, AgentTaskState::Failed)
        }
        _ => return,
    };
    record.updated_at = Some(now_timestamp());
    set_run_state(record, run_state);
    for task in &mut record.tasks {
        if matches!(task.state, AgentTaskState::Queued | AgentTaskState::Running) {
            task.state = task_state;
        }
    }
    let runner_identity = record
        .runner_id()
        .zip(record.runner_job_id())
        .map(|(runner_id, runner_job_id)| (runner_id.to_string(), runner_job_id.to_string()));
    let agent_task_run_id = record.run_id.clone();
    let metadata = record.ensure_metadata_object();
    metadata.insert("runner_job_status".to_string(), json!(status));
    metadata.insert("runner_job_events".to_string(), json!(events));
    if let Some((runner_id, runner_job_id)) = runner_identity {
        metadata.insert(
            "runner_execution_record".to_string(),
            serde_json::to_value(
                homeboy_core::runner_execution_envelope::RunnerExecutionRecord::terminal(
                    &runner_job_id,
                    &runner_id,
                    "daemon",
                    if status == homeboy_core::api_jobs::JobStatus::Succeeded {
                        0
                    } else {
                        1
                    },
                )
                .with_job_id(&runner_job_id)
                .with_agent_task_run_id(agent_task_run_id),
            )
            .unwrap_or(Value::Null),
        );
    }
    metadata.insert(
        "retryable".to_string(),
        json!(run_state != AgentTaskRunState::Succeeded),
    );
    metadata.remove(METADATA_KEY_STALE_RUNNING);
    metadata.remove(METADATA_KEY_STALE_RUNNING_REASON);
}

pub(crate) fn record_runner_job_terminal_metadata(
    record: &mut AgentTaskRunRecord,
    status: homeboy_core::api_jobs::JobStatus,
    events: &[homeboy_core::api_jobs::JobEvent],
) {
    let runner_identity = record
        .runner_id()
        .zip(record.runner_job_id())
        .map(|(runner_id, runner_job_id)| (runner_id.to_string(), runner_job_id.to_string()));
    let agent_task_run_id = record.run_id.clone();
    let metadata = record.ensure_metadata_object();
    metadata.insert("runner_job_status".to_string(), json!(status));
    metadata.insert("runner_job_events".to_string(), json!(events));
    if let Some((runner_id, runner_job_id)) = runner_identity {
        metadata.insert(
            "runner_execution_record".to_string(),
            serde_json::to_value(
                homeboy_core::runner_execution_envelope::RunnerExecutionRecord::terminal(
                    &runner_job_id,
                    &runner_id,
                    "daemon",
                    if status == homeboy_core::api_jobs::JobStatus::Succeeded {
                        0
                    } else {
                        1
                    },
                )
                .with_job_id(&runner_job_id)
                .with_agent_task_run_id(agent_task_run_id),
            )
            .unwrap_or(Value::Null),
        );
    }
}
