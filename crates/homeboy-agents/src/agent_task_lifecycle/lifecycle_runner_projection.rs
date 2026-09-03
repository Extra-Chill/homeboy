//! Terminal runner-result projection for agent-task lifecycle records.
//!
//! When a runner job reaches a terminal state, its durable log snapshot is
//! projected back onto the controller's lifecycle record and aggregate: binding
//! the runner identity, reconciling the child job snapshot, materializing the
//! terminal lifecycle event, and preserving evidence idempotently. This is the
//! read-back half of the Lab handoff, extracted from `lifecycle_ops` so the
//! projection/validation invariants stay reviewable in isolation.

use super::*;

pub(crate) fn reconcile_runner_job_snapshot(
    record: &mut AgentTaskRunRecord,
    snapshot: &homeboy_core::api_jobs::RunnerJobLogSnapshot,
) -> Result<()> {
    reconcile_runner_job_snapshot_in_store(
        &AgentTaskLifecycleStore::from_current_environment()?,
        record,
        snapshot,
    )
}

/// The store-rooted counterpart of [`reconcile_runner_job_snapshot`].
///
/// This is the read-back half of the Lab handoff and it writes: the binding,
/// the live-progress record, the aggregate idempotence read, the terminal
/// aggregate-and-record commit, and the artifact projection underneath it. The
/// idempotence read is the reason none of it may be left ambient — it decides
/// whether an authoritative terminal result is projected at all. Comparing
/// against another home's aggregate would either re-project a result that is
/// already durable here or, worse, skip projecting one because a *different*
/// installation happened to hold a matching aggregate (#7505).
pub(crate) fn reconcile_runner_job_snapshot_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    record: &mut AgentTaskRunRecord,
    snapshot: &homeboy_core::api_jobs::RunnerJobLogSnapshot,
) -> Result<()> {
    // Single owner of pre-acceptance binding: bind a still-pending controller
    // handoff to this snapshot's daemon job before any validation or projection,
    // then advance a freshly-bound queued proxy to running. Every reconcile path
    // (transport-proxy, runner-job-state, terminal-evidence recovery) flows
    // through here, so binding cannot diverge across callers. Both steps no-op
    // once the run is already bound / no longer queued.
    bind_pending_lab_handoff_snapshot_in_store(lifecycle_store, record, snapshot)?;
    if record.state == AgentTaskRunState::Queued {
        set_run_state(record, AgentTaskRunState::Running);
        for task in &mut record.tasks {
            if task.state == AgentTaskState::Queued {
                task.state = AgentTaskState::Running;
            }
        }
    }
    if record.state.is_terminal() {
        // A transport-only terminal result can arrive before the daemon has
        // published the inner agent-task aggregate. Adopt that later evidence
        // when it proves the same controller run rather than losing its patch.
        if let Some(event) = terminal_runner_lifecycle_event(record, snapshot)? {
            let aggregate = projected_runner_aggregate(record, &event.aggregate);
            if lifecycle_store.read_aggregate(&record.run_id).ok().as_ref() != Some(&aggregate) {
                project_terminal_runner_lifecycle_event_in_store(
                    lifecycle_store,
                    record,
                    snapshot,
                    &event,
                )?;
            }
        }
        return Ok(());
    }
    if matches!(
        snapshot.job.status,
        homeboy_core::api_jobs::JobStatus::Succeeded
            | homeboy_core::api_jobs::JobStatus::Failed
            | homeboy_core::api_jobs::JobStatus::Cancelled
    ) {
        if let Some(event) = terminal_runner_lifecycle_event(record, snapshot)? {
            preserve_terminal_runner_identity(record, &event)?;
        }
    }
    validate_runner_job_snapshot(record, snapshot)?;
    let mut reconciled = record.clone();
    reconciled.record_runner_reachable();
    // Typed terminal evidence is authoritative even when a daemon restart left
    // the denormalized job row queued/running. Consume it before that stale row
    // can refresh the controller heartbeat and resurrect the Cook.
    if let Some(event) = terminal_runner_lifecycle_event(&reconciled, snapshot)? {
        project_terminal_runner_lifecycle_event_in_store(
            lifecycle_store,
            &mut reconciled,
            snapshot,
            &event,
        )?;
        *record = reconciled;
        return Ok(());
    }
    if project_terminal_runner_pre_provider_failure_in_store(
        lifecycle_store,
        &mut reconciled,
        &snapshot.events,
    )? {
        *record = reconciled;
        return Ok(());
    }
    match snapshot.job.status {
        homeboy_core::api_jobs::JobStatus::Queued | homeboy_core::api_jobs::JobStatus::Running => {
            reconciled.updated_at = Some(now_timestamp());
            update_lifecycle_heartbeat(&mut reconciled);
            let last_seen_at = reconciled.updated_at.clone();
            let metadata = reconciled.ensure_metadata_object();
            metadata.insert("runner_job_status".to_string(), json!(snapshot.job.status));
            metadata.insert("runner_job_last_seen_at".to_string(), json!(last_seen_at));
            metadata.insert("runner_job_events".to_string(), json!(snapshot.events));
            let queued = snapshot.job.status == homeboy_core::api_jobs::JobStatus::Queued;
            metadata.insert(
                "phase".to_string(),
                json!(if queued {
                    "waiting_for_capacity"
                } else {
                    "executing"
                }),
            );
            metadata.insert(
                "phase_activity".to_string(),
                json!(if queued {
                    "runner owns this FIFO queue entry; awaiting a capacity lease"
                } else {
                    "provider/executor process is active"
                }),
            );
            metadata.insert(
                "provider_state".to_string(),
                json!(if queued { "queued" } else { "active" }),
            );
            metadata.insert(
                "runner_queue".to_string(),
                json!({
                    "owner_runner_id": snapshot.job.target_runner_id,
                    "ordering": "fifo",
                    "dispatch_eligibility": "runner_capacity_lease",
                    "state": if queued { "waiting_for_capacity" } else { "claimed" },
                }),
            );
            if let Some(provider) = metadata
                .get("provider_rotation")
                .and_then(|rotation| rotation.get("entries"))
                .and_then(Value::as_array)
                .and_then(|entries| entries.first())
            {
                metadata.insert("active_provider".to_string(), provider.clone());
            }
            merge_live_provider_handles(&mut reconciled, &snapshot.events);
            lifecycle_store.write_record(&reconciled)?;
        }
        homeboy_core::api_jobs::JobStatus::Succeeded
        | homeboy_core::api_jobs::JobStatus::Failed
        | homeboy_core::api_jobs::JobStatus::Cancelled => {
            record_pending_runner_synchronization(&mut reconciled, snapshot);
            lifecycle_store.write_record(&reconciled)?;
        }
    }
    *record = reconciled;
    Ok(())
}

/// Project an authoritative terminal daemon snapshot into its persisted run.
/// The daemon calls this before returning a foreground `runner exec --run-id`,
/// so its caller never reports a terminal command while the durable run remains
/// active. Replaying the same terminal snapshot is a no-op once projected.
pub fn project_terminal_runner_result(
    run_id: &str,
    snapshot: &homeboy_core::api_jobs::RunnerJobLogSnapshot,
) -> Result<bool> {
    project_terminal_runner_result_in_store(
        &AgentTaskLifecycleStore::from_current_environment()?,
        run_id,
        snapshot,
    )
}

/// The store-rooted counterpart of [`project_terminal_runner_result`].
///
/// Every durable touch below follows the injected store, and this one has two
/// distinct commit targets rather than one, which is why leaving any of it
/// ambient would split it:
///
/// * the runner-exec branch finalizes an *observation* row through
///   `project_terminal_runner_exec_result_in_store`;
/// * the agent-task branch reads the lifecycle record, binds the pending Lab
///   handoff (a durable write, through `record_detached_lab_run_in_store` and
///   its handoff lock on this store's `run_dir`), reads the aggregate to decide
///   idempotence, and then either projects the terminal lifecycle event or
///   writes the transport-only terminal record.
///
/// The aggregate read is the decision, not a report: comparing against another
/// home's aggregate would either re-project a result that is already durable
/// here or skip projecting one because a *different* installation happened to
/// hold a matching aggregate. This function is the daemon's pre-return
/// projection for a foreground `runner exec --run-id`, so that mistake would
/// surface as a terminal command returned over a still-active durable run
/// (#7505).
///
/// The runner-exec branch is opened through `open_observation_maintained`, not
/// the lifecycle opener, so its startup artifact maintenance is unchanged from
/// the ambient path it replaced.
pub fn project_terminal_runner_result_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    snapshot: &homeboy_core::api_jobs::RunnerJobLogSnapshot,
) -> Result<bool> {
    if !matches!(
        snapshot.job.status,
        homeboy_core::api_jobs::JobStatus::Succeeded
            | homeboy_core::api_jobs::JobStatus::Failed
            | homeboy_core::api_jobs::JobStatus::Cancelled
    ) {
        return Ok(false);
    }

    // Ad hoc runner-exec runs are observation records, not agent-task records.
    // Their daemon terminal result is complete without an inner task aggregate.
    if project_terminal_runner_exec_result_in_store(lifecycle_store, run_id, snapshot)? {
        return Ok(true);
    }

    let mut record = lifecycle_store.read_record(&sanitize_run_id(run_id))?;
    // Bind a still-pending controller handoff to this authoritative terminal
    // snapshot's daemon job before validating identity (issue #9240). An
    // accepted Lab job can reach a terminal daemon snapshot before the
    // controller has persisted the accepted runner job id; binding here
    // establishes that identity from the same evidence rather than rejecting a
    // valid terminal snapshot against an empty controller job id. No-ops once
    // the run is already bound.
    bind_pending_lab_handoff_snapshot_in_store(lifecycle_store, &mut record, snapshot)?;
    validate_runner_job_snapshot(&record, snapshot)?;
    if let Some(event) = terminal_runner_lifecycle_event(&record, snapshot)? {
        let aggregate = projected_runner_aggregate(&record, &event.aggregate);
        if lifecycle_store.read_aggregate(&record.run_id).ok().as_ref() == Some(&aggregate) {
            return Ok(false);
        }
        project_terminal_runner_lifecycle_event_in_store(
            lifecycle_store,
            &mut record,
            snapshot,
            &event,
        )?;
        return Ok(true);
    }
    if record.state.is_terminal() {
        return Ok(false);
    }
    project_terminal_runner_job_snapshot(&mut record, snapshot);
    lifecycle_store.write_record(&record)?;
    Ok(true)
}

fn record_pending_runner_synchronization(
    record: &mut AgentTaskRunRecord,
    snapshot: &homeboy_core::api_jobs::RunnerJobLogSnapshot,
) {
    let metadata = record.ensure_metadata_object();
    metadata.insert("runner_job_status".to_string(), json!(snapshot.job.status));
    metadata.insert("runner_job_events".to_string(), json!(snapshot.events));
    metadata.insert(
        "phase".to_string(),
        json!("awaiting_runner_synchronization"),
    );
    metadata.insert(
        "phase_activity".to_string(),
        json!("runner job is terminal; awaiting its authoritative agent-task aggregate"),
    );
    metadata.insert("provider_state".to_string(), json!("synchronizing"));
    metadata.insert(
        "runner_result_synchronization".to_string(),
        json!({
            "state": "pending",
            "runner_job_status": snapshot.job.status,
        }),
    );
}

fn project_terminal_runner_job_snapshot(
    record: &mut AgentTaskRunRecord,
    snapshot: &homeboy_core::api_jobs::RunnerJobLogSnapshot,
) {
    // Only the explicit foreground runner-exec path reaches this helper.
    // Detached reconciliation remains pending until its inner aggregate arrives.
    record.updated_at = Some(now_timestamp());
    let (run_state, task_state, phase) = match snapshot.job.status {
        homeboy_core::api_jobs::JobStatus::Succeeded => (
            AgentTaskRunState::Succeeded,
            AgentTaskState::Succeeded,
            "completed",
        ),
        homeboy_core::api_jobs::JobStatus::Failed => {
            (AgentTaskRunState::Failed, AgentTaskState::Failed, "failed")
        }
        homeboy_core::api_jobs::JobStatus::Cancelled => (
            AgentTaskRunState::Cancelled,
            AgentTaskState::Cancelled,
            "cancelled",
        ),
        homeboy_core::api_jobs::JobStatus::Queued | homeboy_core::api_jobs::JobStatus::Running => {
            return
        }
    };
    set_run_state(record, run_state);
    for task in &mut record.tasks {
        if matches!(task.state, AgentTaskState::Queued | AgentTaskState::Running) {
            task.state = task_state;
        }
    }
    record_runner_job_terminal_metadata(record, snapshot.job.status, &snapshot.events);
    let metadata = record.ensure_metadata_object();
    metadata.insert("phase".to_string(), json!(phase));
    metadata.insert(
        "phase_activity".to_string(),
        json!("authoritative runner daemon result projected"),
    );
    metadata.insert("provider_state".to_string(), json!(phase));
    metadata.insert(
        "runner_result_synchronization".to_string(),
        json!({
            "state": "projected",
            "runner_job_status": snapshot.job.status,
        }),
    );
    if let Some(handoff) = metadata.get_mut("runner_handoff") {
        handoff["state"] = json!("terminal");
    }
    metadata.insert(
        METADATA_KEY_RETRYABLE.to_string(),
        json!(run_state != AgentTaskRunState::Succeeded),
    );
    metadata.remove(METADATA_KEY_STALE_RUNNING);
    metadata.remove(METADATA_KEY_STALE_RUNNING_REASON);
}

/// Extracts the richer inner agent-task aggregate when the terminal daemon
/// result includes one. Generic reconciliation retains a transport-only result
/// as pending; foreground explicit runner execution projects it directly.
fn terminal_runner_lifecycle_event(
    record: &AgentTaskRunRecord,
    snapshot: &homeboy_core::api_jobs::RunnerJobLogSnapshot,
) -> Result<
    Option<crate::agent_task_lifecycle::agent_task_lifecycle_event::AgentTaskRunPlanLifecycleEvent>,
> {
    let runner_id = record.runner_id().unwrap_or_default();
    let runner_job_id = record.runner_job_id().unwrap_or_default();
    if let Some(event) = crate::agent_task_lifecycle::agent_task_lifecycle_event::agent_task_run_plan_lifecycle_event_from_persisted_job_events(
        &snapshot.events,
        runner_id,
        runner_job_id,
        &record.run_id,
    )? {
        return Ok(Some(event));
    }
    Ok(crate::agent_task_lifecycle::agent_task_lifecycle_event::agent_task_run_plan_lifecycle_event_from_job_events(
        Some(&snapshot.events),
    ))
}

/// Project one terminal runner lifecycle event onto its durable record.
///
/// There is deliberately no ambient wrapper any more. Both callers —
/// `reconcile_runner_job_snapshot_in_store` and
/// `project_terminal_runner_result_in_store` — are rooted and already hold the
/// store whose record they are projecting, and this body commits an aggregate,
/// a record, and an artifact projection. An ambient form would exist only to let
/// a rooted caller decide from one installation's evidence and commit into
/// another's (#7505).
///
/// Mirrors `project_persisted_terminal_runner_events_in_store` exactly: the
/// aggregate path stamped onto the record, the combined aggregate-and-record
/// commit, and the terminal artifact projection all follow the injected store.
/// The artifact projection is the one that cannot be left ambient — it
/// registers controller-owned bytes under the artifact root `PathRoots` carries
/// separately from `data`, which is the cross-home artifact write #12618 found
/// (#7505).
fn project_terminal_runner_lifecycle_event_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    record: &mut AgentTaskRunRecord,
    snapshot: &homeboy_core::api_jobs::RunnerJobLogSnapshot,
    event: &crate::agent_task_lifecycle::agent_task_lifecycle_event::AgentTaskRunPlanLifecycleEvent,
) -> Result<()> {
    preserve_terminal_runner_identity(record, event)?;
    validate_runner_job_snapshot(record, snapshot)?;
    validate_terminal_child_identity(record, snapshot, event)?;
    let aggregate = projected_runner_aggregate(record, &event.aggregate);
    let projection_plan = aggregate_projection_plan_from_outcomes(&aggregate);
    let aggregate_path = lifecycle_store
        .aggregate_path(&record.run_id)
        .display()
        .to_string();
    apply_aggregate_to_record(record, &projection_plan, &aggregate, aggregate_path);
    record_verified_lab_placement_outcome(record)?;
    // The aggregate is the task result. A successful enclosing daemon job only
    // proves transport completion, not task success.
    record_runner_job_terminal_metadata(record, snapshot.job.status, &snapshot.events);
    lifecycle_store.write_aggregate_and_record(record, &aggregate)?;
    crate::agent_task_lifecycle::record_terminal_artifact_projection_in_store(
        lifecycle_store,
        record,
        &aggregate,
    )
}

pub(crate) fn project_persisted_terminal_runner_events(
    record: &mut AgentTaskRunRecord,
) -> Result<bool> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    project_persisted_terminal_runner_events_in_store(&lifecycle_store, record)
}

/// The store-rooted counterpart of [`project_persisted_terminal_runner_events`].
///
/// Every durable touch below follows the injected store: the aggregate
/// idempotence read, the aggregate path stamped onto the record, the combined
/// aggregate-and-record commit, and the terminal artifact projection. The last
/// is why this cannot be left half-rooted — the projection registers
/// controller-owned bytes under an artifact root that `PathRoots` carries
/// separately from `data`, which is exactly the cross-home artifact write
/// #12618 found (#7505).
pub(crate) fn project_persisted_terminal_runner_events_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    record: &mut AgentTaskRunRecord,
) -> Result<bool> {
    let Some(runner_job_id) = record.runner_job_id() else {
        return Ok(false);
    };
    let Some(events) = record
        .metadata
        .get("runner_job_events")
        .cloned()
        .and_then(|events| {
            serde_json::from_value::<Vec<homeboy_core::api_jobs::JobEvent>>(events).ok()
        })
        .filter(|events| {
            !events.is_empty()
                && events
                    .iter()
                    .all(|event| event.job_id.to_string() == runner_job_id)
        })
    else {
        return Ok(false);
    };
    let event = crate::agent_task_lifecycle::agent_task_lifecycle_event::agent_task_run_plan_lifecycle_event_from_persisted_job_events(
        &events,
        record.runner_id().unwrap_or_default(),
        runner_job_id,
        &record.run_id,
    )?
    .or_else(|| {
        crate::agent_task_lifecycle::agent_task_lifecycle_event::agent_task_run_plan_lifecycle_event_from_job_events(Some(&events))
    });
    if let Some(event) = event {
        validate_terminal_child_event_identity(record, &event)?;
        let aggregate = projected_runner_aggregate(record, &event.aggregate);
        if lifecycle_store.read_aggregate(&record.run_id).ok().as_ref() == Some(&aggregate) {
            return Ok(false);
        }
        let projection_plan = aggregate_projection_plan_from_outcomes(&aggregate);
        let aggregate_path = lifecycle_store
            .aggregate_path(&record.run_id)
            .display()
            .to_string();
        apply_aggregate_to_record(record, &projection_plan, &aggregate, aggregate_path);
        record_verified_lab_placement_outcome(record)?;
        record.ensure_metadata_object().insert(
            "terminal_transport_recovery".to_string(),
            json!("persisted_runner_job_events"),
        );
        lifecycle_store.write_aggregate_and_record(record, &aggregate)?;
        crate::agent_task_lifecycle::record_terminal_artifact_projection_in_store(
            lifecycle_store,
            record,
            &aggregate,
        )?;
        return Ok(true);
    }

    project_terminal_runner_pre_provider_failure_in_store(lifecycle_store, record, &events)
}

const RUNNER_PRE_PROVIDER_FAILURE_PHASE: &str = "lab_runner_job_pre_provider";

fn project_terminal_runner_pre_provider_failure_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    record: &mut AgentTaskRunRecord,
    events: &[homeboy_core::api_jobs::JobEvent],
) -> Result<bool> {
    if record.state.is_terminal() {
        return Ok(false);
    }
    let Some(event) = events.iter().rev().find(|event| {
        event.kind == homeboy_core::api_jobs::JobEventKind::Error
            && event
                .data
                .as_ref()
                .and_then(|data| data.get("phase"))
                .and_then(Value::as_str)
                == Some("local_child_worker_failed_before_child_identity")
            && event.job_id.to_string() == record.runner_job_id().unwrap_or_default()
    }) else {
        return Ok(false);
    };
    if record
        .metadata
        .pointer("/runner_result_synchronization/source")
        .and_then(Value::as_str)
        == Some("typed_runner_job_error")
        && record
            .metadata
            .pointer("/runner_result_synchronization/event_sequence")
            .and_then(Value::as_u64)
            == Some(event.sequence)
    {
        // The event has already been consumed. Report it handled so the caller
        // does not fall through and re-project the stale queued/running row.
        return Ok(true);
    }
    let data = event.data.as_ref().expect("typed runner error has data");
    let reported_code = data
        .get("error_code")
        .or_else(|| data.get("code"))
        .and_then(Value::as_str)
        .filter(|code| !code.trim().is_empty())
        .unwrap_or("internal.unexpected");
    let message = data
        .get("error")
        .and_then(Value::as_str)
        .or(event.message.as_deref())
        .unwrap_or("Lab runner job failed before provider execution");
    let mut error = Error::new(
        ErrorCode::InternalUnexpected,
        homeboy_core::redaction::redact_string(message),
        json!({
            "field": reported_code,
            "child_reported_error_code": reported_code,
            "child_command_result": {
                "schema": "homeboy/runner-job-terminal-error/v1",
                "runner_job_event_sequence": event.sequence,
                "runner_job_error": homeboy_core::redaction::redact_json(data),
            },
        }),
    )
    .with_retryable(true)
    .with_hint(format!(
        "Retry safely: homeboy agent-task retry {} --run",
        record.run_id
    ));
    error.details["pre_execution_phase"] = json!(RUNNER_PRE_PROVIDER_FAILURE_PHASE);

    let plan = lifecycle_store.read_controller_plan(&record.run_id)?;
    let mut failed = crate::agent_task_lifecycle::record_pre_execution_failure_in_store(
        lifecycle_store,
        &record.run_id,
        &plan,
        RUNNER_PRE_PROVIDER_FAILURE_PHASE,
        &error,
    )?;
    record_runner_job_terminal_metadata(
        &mut failed,
        homeboy_core::api_jobs::JobStatus::Failed,
        events,
    );
    let terminalized = failed.state.is_terminal();
    let run_id = failed.run_id.clone();
    let metadata = failed.ensure_metadata_object();
    metadata.insert(
        "runner_result_synchronization".to_string(),
        json!({
            "state": if terminalized { "projected" } else { "candidate_preserved" },
            "runner_job_status": "failed",
            "source": "typed_runner_job_error",
            "event_sequence": event.sequence,
        }),
    );
    if !terminalized {
        lifecycle_store.write_record(&failed)?;
        *record = failed;
        return Ok(true);
    }
    metadata.insert(
        "phase".to_string(),
        json!(RUNNER_PRE_PROVIDER_FAILURE_PHASE),
    );
    metadata.insert(
        "phase_activity".to_string(),
        json!("runner job failed before provider execution"),
    );
    metadata.insert("provider_state".to_string(), json!("failed"));
    metadata.insert(
        "terminal_transport_recovery".to_string(),
        json!("persisted_runner_job_error"),
    );
    metadata.insert(
        "managed_recovery".to_string(),
        json!({
            "action": "agent_task_retry",
            "command": format!("homeboy agent-task retry {run_id} --run"),
            "reason": RUNNER_PRE_PROVIDER_FAILURE_PHASE,
        }),
    );
    if let Some(handoff) = metadata.get_mut("runner_handoff") {
        handoff["state"] = json!("terminal");
    }
    lifecycle_store.write_record(&failed)?;
    *record = failed;
    Ok(true)
}

/// A detached runner reports its terminal result through reconciliation rather
/// than the synchronous offload return path. Preserve the same canonical
/// placement outcome before writing the controller record.
fn record_verified_lab_placement_outcome(record: &mut AgentTaskRunRecord) -> Result<()> {
    let Some(decision) = record.metadata.get("execution_placement_decision").cloned() else {
        return Ok(());
    };
    let decision: homeboy_lab_runner_contract::ExecutionPlacementDecision =
        serde_json::from_value(decision).map_err(|error| {
            Error::validation_invalid_argument(
                "execution_placement_decision",
                format!("durable run has malformed canonical placement decision: {error}"),
                Some(record.run_id.clone()),
                None,
            )
        })?;
    // A submission stamp is a placeholder authored in the absence of routing.
    // Reconciliation holds no routing decision of its own, so it has nothing to
    // supersede the stamp with — and verifying a Lab result against a
    // placeholder that says "local" would manufacture a contradiction out of
    // missing evidence. Skip exactly as this did before the stamp existed.
    if decision.is_submission_stamp() {
        return Ok(());
    }
    let runner_id = record.runner_id().map(str::to_string).ok_or_else(|| {
        Error::validation_invalid_argument(
            "execution_placement_outcome",
            "terminal Lab runner result has no runner identity",
            Some(record.run_id.clone()),
            None,
        )
    })?;
    let outcome = decision
        .outcome(
            homeboy_lab_runner_contract::EffectiveExecutionPlacement::Lab,
            Some(runner_id),
        )
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "execution_placement_outcome",
                "terminal Lab runner result contradicts the canonical placement decision",
                Some(record.run_id.clone()),
                None,
            )
        })?;
    record.ensure_metadata_object().insert(
        "execution_placement_outcome".to_string(),
        serde_json::to_value(outcome).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize execution placement outcome".to_string()),
            )
        })?,
    );
    Ok(())
}

fn projected_runner_aggregate(
    record: &AgentTaskRunRecord,
    aggregate: &AgentTaskAggregate,
) -> AgentTaskAggregate {
    let mut aggregate = aggregate.clone();
    crate::agent_task_lifecycle::project_runner_evidence_refs(record, &mut aggregate);
    aggregate
}

fn preserve_terminal_runner_identity(
    record: &mut AgentTaskRunRecord,
    event: &crate::agent_task_lifecycle::agent_task_lifecycle_event::AgentTaskRunPlanLifecycleEvent,
) -> Result<()> {
    let identity = &event.identity;
    if identity.runner_id.trim().is_empty()
        || identity.runner_job_id.trim().is_empty()
        || identity.run_id.as_deref() != Some(record.run_id.as_str())
        || identity.persisted_run_id.as_deref() != Some(record.run_id.as_str())
    {
        return Ok(());
    }

    let metadata = record.ensure_metadata_object();
    if metadata
        .get("runner_id")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        metadata.insert("runner_id".to_string(), json!(identity.runner_id));
    }
    if metadata
        .get("runner_job_id")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        metadata.insert("runner_job_id".to_string(), json!(identity.runner_job_id));
    }
    Ok(())
}

fn merge_live_provider_handles(
    record: &mut AgentTaskRunRecord,
    events: &[homeboy_core::api_jobs::JobEvent],
) {
    for handle in events.iter().filter_map(|event| {
        event
            .data
            .as_ref()
            .and_then(|data| {
                data.pointer("/metadata/provider_handle")
                    .or_else(|| data.get("provider_handle"))
            })
            .and_then(provider_handle_from_value)
    }) {
        if record
            .provider_handles
            .iter()
            .any(|existing| existing.provider_run_id == handle.run_id)
        {
            continue;
        }
        record.provider_handles.push(AgentTaskRunProviderHandle {
            kind: handle.kind,
            task_id: handle.task_id,
            backend: handle.backend,
            provider_run_id: handle.run_id,
            stream_uri: handle.stream_uri,
            state: Some(AgentTaskState::Running),
            metadata: handle.metadata,
        });
    }
    if !record.provider_handles.is_empty() {
        record.lifecycle.provider_runtime = record
            .provider_handles
            .iter()
            .map(provider_runtime_for_handle)
            .collect();
        record.lifecycle.refresh_external_runtime_ids();
    }
}

fn validate_runner_job_snapshot(
    record: &AgentTaskRunRecord,
    snapshot: &homeboy_core::api_jobs::RunnerJobLogSnapshot,
) -> Result<()> {
    // Build the canonical run/runner/job identity the controller expects, so
    // this validator names the same tuple as every other handoff-identity site
    // (`validate_terminal_child_identity`, `accepted_lab_runner_handoff_identity`,
    // …). This is a snapshot-scoped check: only the `runner_job_id` field is
    // compared against the daemon snapshot; the run/runner fields are carried
    // for diagnostics and mismatch descriptions.
    let expected = homeboy_core::lab_contract::RunnerJobIdentity::new(
        record.run_id.as_str(),
        record.runner_id().unwrap_or_default(),
        record.runner_job_id().unwrap_or_default(),
    );
    // Distinguish "controller job identity not yet established" from a genuine
    // mismatch. When the controller has never persisted an accepted runner job
    // id, the job field is empty and comparing a valid runner snapshot UUID
    // against it would spuriously reject it as a mismatch (issue #9240: "runner
    // snapshot job <uuid> does not match controller job " — the blank trailing
    // value is the missing-identity signal, not a mismatch). This is scoped to
    // the runner-job field alone (matching the pre-migration `runner_job_id()`
    // guard); callers must bind or propagate the accepted Lab job id before
    // validation, so the absence is surfaced as its own diagnostic instead of
    // being presented as a runner mismatch and classified non-retryable.
    if expected.runner_job_id.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "runner_job_id",
            format!(
                "controller run has no accepted runner job identity to validate runner snapshot job {} against; \
                 the accepted Lab handoff job id must be bound before snapshot validation",
                snapshot.job.id
            ),
            Some(record.run_id.clone()),
            None,
        ));
    }
    if expected.runner_job_id == snapshot.job.id.to_string() {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "runner_job_id",
        format!(
            "runner snapshot job {} does not match controller job {}",
            snapshot.job.id, expected.runner_job_id
        ),
        Some(record.run_id.clone()),
        None,
    ))
}

fn validate_terminal_child_identity(
    record: &AgentTaskRunRecord,
    snapshot: &homeboy_core::api_jobs::RunnerJobLogSnapshot,
    event: &crate::agent_task_lifecycle::agent_task_lifecycle_event::AgentTaskRunPlanLifecycleEvent,
) -> Result<()> {
    validate_terminal_child_event_identity(record, event)?;
    if snapshot.job.id.to_string() == record.runner_job_id().unwrap_or_default() {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "runner_lifecycle_identity",
        "terminal runner child lifecycle event does not match its controller run, persisted run, runner, and job identity",
        Some(record.run_id.clone()),
        None,
    ))
}

fn validate_terminal_child_event_identity(
    record: &AgentTaskRunRecord,
    event: &crate::agent_task_lifecycle::agent_task_lifecycle_event::AgentTaskRunPlanLifecycleEvent,
) -> Result<()> {
    // Canonical run/runner/job identity the controller expects, and the identity
    // the terminal event carries (built from its *persisted* run id). Comparing
    // via the shared `RunnerJobIdentity` keeps the runner/job/run tuple check in
    // lockstep with every other handoff-identity site.
    let expected = homeboy_core::lab_contract::RunnerJobIdentity::new(
        record.run_id.as_str(),
        record.runner_id().unwrap_or_default(),
        record.runner_job_id().unwrap_or_default(),
    );
    let event_identity = homeboy_core::lab_contract::RunnerJobIdentity::new(
        event.identity.persisted_run_id.clone().unwrap_or_default(),
        event.identity.runner_id.clone(),
        event.identity.runner_job_id.clone(),
    );
    // Beyond the run/runner/job tuple, the terminal event must also agree on the
    // raw transport run id (not just the persisted run id).
    if expected.matches(&event_identity)
        && event.identity.run_id.as_deref() == Some(record.run_id.as_str())
    {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "runner_lifecycle_identity",
        "terminal runner child lifecycle event does not match its controller run, persisted run, runner, and job identity",
        Some(record.run_id.clone()),
        None,
    ))
}

pub(crate) fn aggregate_projection_plan(
    plan: &AgentTaskPlan,
    aggregate: &AgentTaskAggregate,
) -> AgentTaskPlan {
    if aggregate.outcomes.iter().all(|outcome| {
        plan.tasks
            .iter()
            .any(|task| task.task_id == outcome.task_id)
    }) {
        return plan.clone();
    }
    aggregate_projection_plan_from_outcomes(aggregate)
}

fn aggregate_projection_plan_from_outcomes(aggregate: &AgentTaskAggregate) -> AgentTaskPlan {
    let tasks = aggregate
        .outcomes
        .iter()
        .map(|outcome| AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: outcome.task_id.clone(),
            group_key: Some("runner-child".to_string()),
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: outcome
                    .metadata
                    .get("provider")
                    .and_then(Value::as_str)
                    .unwrap_or("runner-child")
                    .to_string(),
                selector: None,
                runtime_selection: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model: None,
                config: Value::Null,
            },
            instructions: outcome.summary.clone().unwrap_or_default(),
            inputs: Value::Null,
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace::default(),
            component_contracts: Vec::new(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: Vec::new(),
            artifact_declarations: Vec::new(),
            output_declarations: Vec::new(),
            runtime_tools: Vec::new(),
            metadata: outcome.metadata.clone(),
        })
        .collect();
    AgentTaskPlan::new(&aggregate.plan_id, tasks)
}
