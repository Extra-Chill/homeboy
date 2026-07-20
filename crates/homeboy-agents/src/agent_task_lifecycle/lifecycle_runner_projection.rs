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
    if record.state.is_terminal() {
        // A transport-only terminal result can arrive before the daemon has
        // published the inner agent-task aggregate. Adopt that later evidence
        // when it proves the same controller run rather than losing its patch.
        if let Some(event) = terminal_runner_lifecycle_event(record, snapshot)? {
            if store::read_aggregate(&record.run_id).ok().as_ref() != Some(&event.aggregate) {
                project_terminal_runner_lifecycle_event(record, snapshot, &event)?;
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
            store::write_record(&reconciled)?;
        }
        homeboy_core::api_jobs::JobStatus::Succeeded
        | homeboy_core::api_jobs::JobStatus::Failed
        | homeboy_core::api_jobs::JobStatus::Cancelled => {
            if let Some(event) = terminal_runner_lifecycle_event(&reconciled, snapshot)? {
                project_terminal_runner_lifecycle_event(&mut reconciled, snapshot, &event)?;
            } else {
                record_pending_runner_synchronization(&mut reconciled, snapshot);
                store::write_record(&reconciled)?;
            }
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
    if !matches!(
        snapshot.job.status,
        homeboy_core::api_jobs::JobStatus::Succeeded
            | homeboy_core::api_jobs::JobStatus::Failed
            | homeboy_core::api_jobs::JobStatus::Cancelled
    ) {
        return Ok(false);
    }

    let mut record = store::read_record(&sanitize_run_id(run_id))?;
    validate_runner_job_snapshot(&record, snapshot)?;
    if let Some(event) = terminal_runner_lifecycle_event(&record, snapshot)? {
        if store::read_aggregate(&record.run_id).ok().as_ref() == Some(&event.aggregate) {
            return Ok(false);
        }
        project_terminal_runner_lifecycle_event(&mut record, snapshot, &event)?;
        return Ok(true);
    }
    if record.state.is_terminal() {
        return Ok(false);
    }
    project_terminal_runner_job_snapshot(&mut record, snapshot);
    store::write_record(&record)?;
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

fn project_terminal_runner_lifecycle_event(
    record: &mut AgentTaskRunRecord,
    snapshot: &homeboy_core::api_jobs::RunnerJobLogSnapshot,
    event: &crate::agent_task_lifecycle::agent_task_lifecycle_event::AgentTaskRunPlanLifecycleEvent,
) -> Result<()> {
    preserve_terminal_runner_identity(record, event)?;
    validate_runner_job_snapshot(record, snapshot)?;
    validate_terminal_child_identity(record, snapshot, event)?;
    let projection_plan = aggregate_projection_plan_from_outcomes(&event.aggregate);
    let aggregate_path = store::aggregate_path(&record.run_id)
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "aggregate.json".to_string());
    apply_aggregate_to_record(record, &projection_plan, &event.aggregate, aggregate_path);
    // The aggregate is the task result. A successful enclosing daemon job only
    // proves transport completion, not task success.
    record_runner_job_terminal_metadata(record, snapshot.job.status, &snapshot.events);
    store::write_aggregate_and_record(record, &event.aggregate)?;
    crate::agent_task_lifecycle::record_terminal_artifact_projection(record, &event.aggregate)
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
        record.lifecycle.external_runtime_ids = record
            .lifecycle
            .provider_runtime
            .iter()
            .flat_map(|runtime| runtime.external_runtime_ids.clone())
            .collect();
    }
}

fn validate_runner_job_snapshot(
    record: &AgentTaskRunRecord,
    snapshot: &homeboy_core::api_jobs::RunnerJobLogSnapshot,
) -> Result<()> {
    let expected_job_id = record.runner_job_id().unwrap_or_default();
    if expected_job_id == snapshot.job.id.to_string() {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "runner_job_id",
        format!(
            "runner snapshot job {} does not match controller job {expected_job_id}",
            snapshot.job.id
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
    let expected_runner_id = record.runner_id().unwrap_or_default();
    let expected_job_id = record.runner_job_id().unwrap_or_default();
    let expected_run_id = record.run_id.as_str();
    if event.identity.runner_id == expected_runner_id
        && event.identity.runner_job_id == expected_job_id
        && snapshot.job.id.to_string() == expected_job_id
        && event.identity.run_id.as_deref() == Some(expected_run_id)
        && event.identity.persisted_run_id.as_deref() == Some(expected_run_id)
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
            metadata: outcome.metadata.clone(),
        })
        .collect();
    AgentTaskPlan::new(&aggregate.plan_id, tasks)
}
