use super::*;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};

/// Merge a completed deferred-cleanup candidate into its timeout outcome.
///
/// The worker owns the mutable workspace until it exits; this lifecycle-side
/// operation is the only place where its immutable recovery result is adopted.
/// A per-run advisory lock makes concurrent status/artifact/Cook readers
/// reread and persist one coherent aggregate and terminal projection.
pub fn reconcile_deferred_candidate(run_id: &str) -> Result<bool> {
    let run_id = resolve_run_id(run_id)?;
    let lock_path = paths::homeboy_data()?
        .join("agent-task-runs")
        .join(&run_id)
        .join("deferred-candidate.lock");
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent).map_err(|error| Error::internal_io(error.to_string(), None))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("open deferred candidate lock".to_string()),
            )
        })?;
    let _lock = DeferredCandidateLock::lock(file)?;

    let mut record = store::read_record(&run_id)?;
    let mut aggregate = match store::read_aggregate(&run_id) {
        Ok(aggregate) => aggregate,
        // The worker may finish before the aggregate is committed. A later
        // read retries from durable state rather than inventing a projection.
        Err(_) => return Ok(false),
    };
    let plan = store::read_plan_path(&record.plan_path)?;
    let mut changed = false;

    for outcome in &mut aggregate.outcomes {
        if outcome.status != AgentTaskOutcomeStatus::Timeout {
            continue;
        }
        let Some(action) = outcome.artifacts.iter().find(|artifact| {
            artifact.schema == crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA
                && artifact.kind == "cleanup_action"
                && artifact.role.as_deref() == Some("cleanup_action")
                && artifact.metadata.get("run_id").and_then(Value::as_str) == Some(run_id.as_str())
                && artifact.metadata.get("task_id").and_then(Value::as_str)
                    == Some(outcome.task_id.as_str())
        }) else {
            continue;
        };
        let Some(path) = action.path.as_deref() else {
            continue;
        };
        let raw = match fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(_) => continue,
        };
        let action_value: Value = match serde_json::from_str(&raw) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if action_value.get("schema").and_then(Value::as_str)
            != Some("homeboy/agent-task-deferred-cleanup/v1")
            || action_value.get("run_id").and_then(Value::as_str) != Some(run_id.as_str())
            || action_value.get("task_id").and_then(Value::as_str) != Some(outcome.task_id.as_str())
            || action_value.get("attempt") != action.metadata.get("attempt")
        {
            continue;
        }
        match action_value.get("status").and_then(Value::as_str) {
            Some("pending") | Some("completed") | Some("completed_no_candidate") | None => continue,
            Some("failed") => {
                let diagnostic = action_value
                    .get("diagnostic")
                    .and_then(Value::as_str)
                    .unwrap_or("deferred cleanup failed");
                if !outcome
                    .diagnostics
                    .iter()
                    .any(|entry| entry.class == "agent_task.deferred_cleanup_failed")
                {
                    outcome.diagnostics.push(AgentTaskDiagnostic {
                        class: "agent_task.deferred_cleanup_failed".to_string(),
                        message: diagnostic.chars().take(512).collect(),
                        data: json!({ "safe_next_action": "Inspect the deferred cleanup diagnostic before retrying the provider." }),
                    });
                    changed = true;
                }
            }
            Some("candidate_recovered") => {
                let Some(candidates) = action_value
                    .get("candidate_artifacts")
                    .and_then(Value::as_array)
                else {
                    continue;
                };
                let mut recovered = Vec::new();
                for value in candidates {
                    let Ok(artifact) = serde_json::from_value::<AgentTaskArtifact>(value.clone())
                    else {
                        continue;
                    };
                    let portable = artifact.url.as_deref()
                        == Some(&candidate_artifact_url(
                            &run_id,
                            &outcome.task_id,
                            &artifact.id,
                        ));
                    let valid_sha = artifact.sha256.as_deref().is_some_and(|sha| {
                        sha.len() == 64 && sha.bytes().all(|byte| byte.is_ascii_hexdigit())
                    });
                    let content_matches = artifact
                        .path
                        .as_deref()
                        .and_then(|candidate_path| fs::read(candidate_path).ok())
                        .is_some_and(|bytes| {
                            let actual = format!("{:x}", Sha256::digest(bytes));
                            artifact.sha256.as_deref() == Some(actual.as_str())
                        });
                    if artifact.schema == crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA
                        && portable
                        && valid_sha
                        && content_matches
                        && crate::agent_task_timeout_artifacts::is_actionable_patch_artifact(
                            &artifact,
                        )
                    {
                        recovered.push(artifact);
                    }
                }
                if recovered.is_empty() {
                    continue;
                }
                crate::agent_task_timeout_artifacts::append_unique_artifacts(
                    &mut outcome.artifacts,
                    recovered,
                );
                outcome.status = AgentTaskOutcomeStatus::CandidateRecoverable;
                outcome.failure_classification = None;
                outcome.summary =
                    Some("deferred cleanup recovered a canonical patch candidate".to_string());
                outcome.metadata["deferred_candidate_reconciled_at"] = json!(now_timestamp());
                outcome.metadata["safe_next_action"] =
                    json!("Promote the recovered candidate through controller-owned gates.");
                changed = true;
            }
            Some(_) => continue,
        }
    }
    if !changed {
        return Ok(false);
    }

    aggregate.status = aggregate_status(&aggregate.outcomes);
    aggregate.totals = aggregate_totals(plan.tasks.len(), &aggregate.outcomes);
    let aggregate_path = store::aggregate_path(&run_id)?.display().to_string();
    apply_aggregate_to_record(&mut record, &plan, &aggregate, aggregate_path);
    store::write_aggregate_and_record(&record, &aggregate)?;
    record_terminal_artifact_projection(&mut record, &aggregate)?;
    Ok(true)
}

fn candidate_artifact_url(run_id: &str, task_id: &str, artifact_id: &str) -> String {
    use crate::execution_contract::encode_uri_component;

    format!(
        "homeboy://agent-task/run/{}/artifacts#task={}&artifact={}",
        encode_uri_component(run_id),
        encode_uri_component(task_id),
        encode_uri_component(artifact_id),
    )
}

struct DeferredCandidateLock {
    #[allow(dead_code)] // Retains the advisory lock until this guard drops.
    file: File,
}
impl DeferredCandidateLock {
    #[cfg(unix)]
    fn lock(file: File) -> Result<Self> {
        use std::os::fd::AsRawFd;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(Error::internal_io(
                std::io::Error::last_os_error().to_string(),
                Some("lock deferred candidate".to_string()),
            ));
        }
        Ok(Self { file })
    }
    #[cfg(not(unix))]
    fn lock(file: File) -> Result<Self> {
        Ok(Self { file })
    }
}

fn aggregate_status(outcomes: &[AgentTaskOutcome]) -> AgentTaskAggregateStatus {
    if outcomes
        .iter()
        .any(|outcome| outcome.status == AgentTaskOutcomeStatus::Cancelled)
    {
        return AgentTaskAggregateStatus::Cancelled;
    }
    if outcomes
        .iter()
        .any(|outcome| outcome.status == AgentTaskOutcomeStatus::CandidateRecoverable)
    {
        return AgentTaskAggregateStatus::PartialRecoverable;
    }
    let succeeded = outcomes.iter().any(|outcome| {
        matches!(
            outcome.status,
            AgentTaskOutcomeStatus::Succeeded | AgentTaskOutcomeStatus::NoOp
        )
    });
    let failed = outcomes.iter().any(|outcome| {
        !matches!(
            outcome.status,
            AgentTaskOutcomeStatus::Succeeded | AgentTaskOutcomeStatus::NoOp
        )
    });
    match (succeeded, failed) {
        (true, false) => AgentTaskAggregateStatus::Succeeded,
        (true, true) => AgentTaskAggregateStatus::PartialFailure,
        _ => AgentTaskAggregateStatus::Failed,
    }
}

fn aggregate_totals(total_tasks: usize, outcomes: &[AgentTaskOutcome]) -> AgentTaskAggregateTotals {
    let mut totals = AgentTaskAggregateTotals {
        queued: total_tasks.saturating_sub(outcomes.len()),
        ..Default::default()
    };
    for outcome in outcomes {
        match outcome.status {
            AgentTaskOutcomeStatus::Succeeded | AgentTaskOutcomeStatus::NoOp => {
                totals.succeeded += 1
            }
            AgentTaskOutcomeStatus::Timeout => totals.timed_out += 1,
            AgentTaskOutcomeStatus::Cancelled => totals.cancelled += 1,
            AgentTaskOutcomeStatus::CandidateRecoverable => totals.recoverable_candidates += 1,
            _ => totals.failed += 1,
        }
    }
    totals
}

pub fn submit_plan(
    plan: &AgentTaskPlan,
    requested_run_id: Option<&str>,
) -> Result<AgentTaskRunRecord> {
    let run_id = requested_run_id
        .map(sanitize_run_id)
        .unwrap_or_else(default_run_id);
    let plan_path = store::write_plan(&run_id, plan)?;

    let mut metadata = json!({
        "task_count": plan.tasks.len(),
        "max_concurrency": plan.options.max_concurrency,
        "provider_run_ids": [],
        "lifecycle_schema": RUN_LIFECYCLE_RECORD_SCHEMA,
        "note": "submitted tasks are durable; provider run ids are recorded after an executor returns them as generic artifacts or evidence refs"
    });
    metadata[crate::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY] =
        crate::controller_runtime::pin_current()?;
    if let Ok(runner_id) = std::env::var(crate::runner::RUNNER_ID_ENV) {
        if !runner_id.trim().is_empty() {
            metadata["runner_id"] = json!(runner_id);
        }
    }
    if let Some(route) = crate::notification_route::current() {
        route.insert_into_metadata(&mut metadata);
    }

    let record = AgentTaskRunRecord {
        schema: schemas::RUN.to_string(),
        run_id,
        plan_id: plan.plan_id.clone(),
        state: AgentTaskRunState::Queued,
        submitted_at: now_timestamp(),
        updated_at: None,
        plan_path: plan_path.display().to_string(),
        aggregate_path: None,
        totals: None,
        tasks: plan.tasks.iter().map(queued_task).collect(),
        artifact_refs: Vec::new(),
        provider_handles: Vec::new(),
        latest_executor_evidence: None,
        lifecycle: lifecycle_for_submitted_plan(plan),
        metadata,
    };
    store::write_record(&record)?;
    Ok(record)
}

/// Bind an inherited route when a detached workload recreates an agent-task run.
pub fn persist_notification_route(
    run_id: &str,
    route: &crate::notification_route::NotificationRoute,
) -> Result<()> {
    let mut record = store::read_record(run_id)?;
    route.insert_into_metadata(&mut record.metadata);
    store::write_record(&record)
}

pub fn record_completed_run(
    plan: &AgentTaskPlan,
    aggregate: &AgentTaskAggregate,
    requested_run_id: Option<&str>,
) -> Result<AgentTaskRunRecord> {
    let mut record = submit_plan(plan, requested_run_id)?;
    record_aggregate(&mut record, plan, aggregate)
}

pub fn load_plan(run_id: &str) -> Result<AgentTaskPlan> {
    let record = store::read_record(&resolve_run_id(run_id)?)?;
    store::read_plan_path(&record.plan_path)
}

/// Load a durable plan for a scheduler or provider execution. This is the only
/// read path allowed to upgrade a legacy execution-budget envelope.
pub fn load_plan_for_execution(run_id: &str) -> Result<AgentTaskPlan> {
    let record = store::read_record(&resolve_run_id(run_id)?)?;
    store::read_plan_path_for_execution(&record.plan_path)
}

pub fn mark_running(run_id: &str) -> Result<AgentTaskRunRecord> {
    let mut record = store::read_record(&sanitize_run_id(run_id))?;
    crate::controller_runtime::validate_for_mutation(
        &record.metadata,
        &crate::build_identity::current().display,
    )?;
    if record.state == AgentTaskRunState::Running && record.owner_process_is_running() {
        return Err(Error::validation_invalid_argument(
            "run_id",
            format!(
                "agent-task run '{}' is already running under pid {}",
                record.run_id,
                record.owner_pid().unwrap_or_default()
            ),
            Some(record.run_id),
            None,
        ));
    }
    if matches!(
        record.state,
        AgentTaskRunState::Succeeded
            | AgentTaskRunState::PartialRecoverable
            | AgentTaskRunState::PartialFailure
            | AgentTaskRunState::Failed
            | AgentTaskRunState::Cancelled
    ) {
        return Err(Error::validation_invalid_argument(
            "run_id",
            format!(
                "agent-task run '{}' is already terminal with state {:?}",
                record.run_id, record.state
            ),
            Some(record.run_id),
            None,
        ));
    }

    let reclaimed_stale = record.state == AgentTaskRunState::Running;
    record.updated_at = Some(now_timestamp());
    set_run_state(&mut record, AgentTaskRunState::Running);
    update_lifecycle_heartbeat(&mut record);
    for task in &mut record.tasks {
        if task.state == AgentTaskState::Queued {
            task.state = AgentTaskState::Running;
        }
    }
    record.record_runner_metadata(reclaimed_stale);
    store::write_record(&record)?;
    Ok(record)
}

#[cfg(test)]
pub(crate) fn rewrite_record_for_test<F>(run_id: &str, mut rewrite: F) -> Result<AgentTaskRunRecord>
where
    F: FnMut(&mut AgentTaskRunRecord),
{
    let mut record = store::read_record(&sanitize_run_id(run_id))?;
    rewrite(&mut record);
    store::write_record(&record)?;
    Ok(record)
}

pub fn claim_next_queued_run() -> Result<Option<AgentTaskRunRecord>> {
    let mut queued: Vec<AgentTaskRunRecord> = store::read_records()?
        .into_iter()
        .filter(|record| record.state == AgentTaskRunState::Queued && !is_transport_proxy(record))
        .collect();
    queued.sort_by(|left, right| {
        left.submitted_at
            .cmp(&right.submitted_at)
            .then_with(|| left.run_id.cmp(&right.run_id))
    });

    for record in queued {
        match mark_running(&record.run_id) {
            Ok(claimed) => return Ok(Some(claimed)),
            Err(error) if error.code == ErrorCode::ValidationInvalidArgument => continue,
            Err(error) => return Err(error),
        }
    }

    Ok(None)
}

pub fn record_run_aggregate(
    run_id: &str,
    plan: &AgentTaskPlan,
    aggregate: &AgentTaskAggregate,
) -> Result<AgentTaskRunRecord> {
    let mut record = store::read_record(&sanitize_run_id(run_id))?;
    record_aggregate(&mut record, plan, aggregate)
}

/// Retain the runner-side job identity after a completed remote aggregate is
/// mirrored so the controller record remains joinable to daemon evidence.
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

pub fn status(run_id: &str) -> Result<AgentTaskRunRecord> {
    let requested_run_id = sanitize_run_id(run_id);
    let resolved_run_id = resolve_run_id(run_id)?;
    let _ = reconcile_deferred_candidate(&resolved_run_id)?;
    let mut record = store::read_record(&resolved_run_id)?;
    if !is_terminal_run_state(record.state) {
        if let (Ok(aggregate), Ok(plan)) = (
            store::read_aggregate(&record.run_id),
            store::read_plan_path(&record.plan_path),
        ) {
            let aggregate_path = store::aggregate_path(&record.run_id)
                .map(|path| path.display().to_string())
                .unwrap_or_else(|_| "aggregate.json".to_string());
            let mut reconciled = record.clone();
            let projection_plan = aggregate_projection_plan(&plan, &aggregate);
            apply_aggregate_to_record(
                &mut reconciled,
                &projection_plan,
                &aggregate,
                aggregate_path,
            );

            if reconciled != record {
                if let Err(error) = store::write_record(&reconciled) {
                    reconciled
                        .ensure_metadata_object()
                        .insert("finalization_error".to_string(), json!(error.message));
                }

                record = reconciled;
            }
        }
    }
    let before_liveness_reconciliation = record.clone();
    reconcile_runner_job_state(&mut record)?;
    record.annotate_stale_running();
    if record != before_liveness_reconciliation {
        store::write_record(&record)?;
    }
    if requested_run_id != record.run_id {
        if let Ok(index) = store::read_cook_index(&requested_run_id) {
            let metadata = record.ensure_metadata_object();
            metadata.insert("cook_alias".to_string(), json!(requested_run_id));
            metadata.insert(
                "cook_index".to_string(),
                serde_json::to_value(index).unwrap_or(Value::Null),
            );
        }
    }
    if is_terminal_run_state(record.state)
        && record
            .metadata
            .get("artifact_projection")
            .and_then(Value::as_object)
            .and_then(|value| value.get("status"))
            .and_then(Value::as_str)
            != Some("complete")
    {
        if let Ok(aggregate) = store::read_aggregate(&record.run_id) {
            crate::agent_task_lifecycle::record_terminal_artifact_projection(
                &mut record,
                &aggregate,
            )?;
        }
    }
    // Read-side reconciliation only writes the durable continuation signal.
    // The separate consumer owns execution and cannot inherit a local closure.
    if matches!(
        record.state,
        AgentTaskRunState::Succeeded
            | AgentTaskRunState::CandidateRecoverable
            | AgentTaskRunState::PartialRecoverable
    ) {
        if let Some(cook_id) = record
            .metadata
            .get("cook_id")
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            if crate::agent_task_service::recipe_exists(&cook_id)? {
                if let Err(error) = crate::agent_task_service::enqueue_terminal_continuation(
                    &cook_id,
                    &record.run_id,
                ) {
                    record.ensure_metadata_object().insert(
                        "cook_continuation_scheduler".to_string(),
                        json!({
                            "status": "failed",
                            "error_code": error.code.as_str(),
                            "message": error.message,
                        }),
                    );
                    store::write_record(&record)?;
                }
            }
        }
    }
    Ok(record)
}

/// Refresh every accepted runner handoff before a read model (such as activity)
/// projects lifecycle state. A controller wait expiry is not terminal: the
/// runner daemon remains the authority until it reports a terminal job result.
pub fn reconcile_active_runner_handoffs() -> Result<usize> {
    let run_ids = list_records()?
        .into_iter()
        .filter(|record| {
            record.state == AgentTaskRunState::Running
                && record.runner_id().is_some()
                && record.runner_job_id().is_some()
        })
        .map(|record| record.run_id)
        .collect::<Vec<_>>();
    let mut reconciled = 0;
    for run_id in run_ids {
        // `status` owns snapshot validation, persistence, and the exact
        // no-PID daemon-loss projection. A bad remote record must not prevent
        // unrelated activity from being listed.
        if status(&run_id).is_ok() {
            reconciled += 1;
        }
    }
    Ok(reconciled)
}

fn reconcile_runner_job_state(record: &mut AgentTaskRunRecord) -> Result<()> {
    if record.state != AgentTaskRunState::Running {
        return Ok(());
    }
    let (Some(runner_id), Some(job_id)) = (
        record.runner_id().map(str::to_string),
        record.runner_job_id().map(str::to_string),
    ) else {
        return Ok(());
    };
    let snapshot = match crate::runners::runner_job_log_snapshot(&runner_id, &job_id) {
        Ok(snapshot) => snapshot,
        Err(_) => {
            if crate::runners::status(&runner_id)
                .map(|status| !status.connected)
                .unwrap_or(false)
            {
                record.annotate_runner_disconnected();
            }
            return Ok(());
        }
    };
    reconcile_runner_job_snapshot(record, &snapshot)
}

/// A controller proxy is a transport handoff, not an agent provider task. Its
/// synthetic plan task identifies the runner so the durable record remains
/// inspectable, but must never reach provider lookup on the controller.
#[derive(Debug, Clone, PartialEq)]
pub enum TransportProxyRecovery {
    Resumed {
        record: AgentTaskRunRecord,
        next_action: String,
    },
    Reconciled {
        record: AgentTaskRunRecord,
        next_action: String,
    },
    ReconnectRequired {
        record: AgentTaskRunRecord,
        next_action: String,
    },
}

impl TransportProxyRecovery {
    pub fn record(&self) -> &AgentTaskRunRecord {
        match self {
            Self::Resumed { record, .. }
            | Self::Reconciled { record, .. }
            | Self::ReconnectRequired { record, .. } => record,
        }
    }

    pub fn next_action(&self) -> &str {
        match self {
            Self::Resumed { next_action, .. }
            | Self::Reconciled { next_action, .. }
            | Self::ReconnectRequired { next_action, .. } => next_action,
        }
    }
}

/// Reconnect a transport-owned proxy through its durable runner execution
/// record. `None` means the run is a normal scheduler-owned plan.
pub fn recover_transport_proxy(run_id: &str) -> Result<Option<TransportProxyRecovery>> {
    let mut record = store::read_record(&sanitize_run_id(run_id))?;
    crate::controller_runtime::validate_for_mutation(
        &record.metadata,
        &crate::build_identity::current().display,
    )?;
    if !is_transport_proxy(&record) {
        return Ok(None);
    }

    let Some(runner_id) = transport_proxy_runner_id(&record) else {
        return Ok(None);
    };
    let runner_job_id = transport_proxy_runner_job_id(&record);
    let metadata = record.ensure_metadata_object();
    metadata.insert("retryable".to_string(), json!(true));
    metadata.insert("transport_recovery".to_string(), json!("required"));
    metadata.insert("runner_id".to_string(), json!(&runner_id));

    let Some(runner_job_id) = runner_job_id else {
        return resume_transport_proxy_on_runner(record, runner_id);
    };

    metadata.insert("runner_job_id".to_string(), json!(&runner_job_id));

    match crate::runners::runner_job_log_snapshot(&runner_id, &runner_job_id) {
        Ok(snapshot) => {
            reconcile_transport_proxy_snapshot(&mut record, &snapshot)?;
            store::write_record(&record)?;
            Ok(Some(TransportProxyRecovery::Reconciled {
                record,
                next_action: format!(
                    "homeboy runner job logs {runner_id} {runner_job_id} --follow"
                ),
            }))
        }
        Err(_) => {
            record.annotate_runner_disconnected();
            store::write_record(&record)?;
            Ok(Some(TransportProxyRecovery::ReconnectRequired {
                record,
                next_action: format!("homeboy runner connect {runner_id}"),
            }))
        }
    }
}

/// Resume an unbound proxy where the controller never learned the runner job
/// identity. The persisted command and workspace belong to the runner, so this
/// must execute through the runner rather than through the local scheduler.
fn resume_transport_proxy_on_runner(
    mut record: AgentTaskRunRecord,
    runner_id: String,
) -> Result<Option<TransportProxyRecovery>> {
    let Some(remote_workspace) = record
        .metadata
        .get("remote_workspace")
        .and_then(Value::as_str)
        .filter(|path| !path.trim().is_empty())
    else {
        store::write_record(&record)?;
        return Ok(Some(TransportProxyRecovery::ReconnectRequired {
            record,
            next_action: format!("homeboy runner connect {runner_id}"),
        }));
    };
    let Some(remote_command) = record
        .metadata
        .get("remote_command")
        .and_then(Value::as_array)
        .map(|command| {
            command
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|command| !command.is_empty())
    else {
        store::write_record(&record)?;
        return Ok(Some(TransportProxyRecovery::ReconnectRequired {
            record,
            next_action: format!("homeboy runner connect {runner_id}"),
        }));
    };

    if !crate::runners::exists(&runner_id) {
        store::write_record(&record)?;
        return Ok(Some(TransportProxyRecovery::ReconnectRequired {
            record,
            next_action: format!("homeboy runner connect {runner_id}"),
        }));
    }

    let (_, exit_code) = crate::runners::exec(
        &runner_id,
        crate::runners::RunnerExecOptions {
            cwd: Some(remote_workspace.to_string()),
            command: remote_command,
            run_id: Some(record.run_id.clone()),
            ..Default::default()
        },
    )?;
    if exit_code != 0 {
        return Err(Error::internal_unexpected(format!(
            "runner continuation for agent-task run '{}' exited with status {exit_code}",
            record.run_id
        )));
    }

    record = store::read_record(&record.run_id)?;
    Ok(Some(TransportProxyRecovery::Resumed {
        next_action: format!("homeboy agent-task status {} --full", record.run_id),
        record,
    }))
}

pub(crate) fn reconcile_transport_proxy_snapshot(
    record: &mut AgentTaskRunRecord,
    snapshot: &crate::runner::RunnerJobLogSnapshot,
) -> Result<()> {
    if record.state == AgentTaskRunState::Queued {
        set_run_state(record, AgentTaskRunState::Running);
        for task in &mut record.tasks {
            if task.state == AgentTaskState::Queued {
                task.state = AgentTaskState::Running;
            }
        }
    }
    reconcile_runner_job_snapshot(record, snapshot)
}

fn is_transport_proxy(record: &AgentTaskRunRecord) -> bool {
    record
        .metadata
        .get("kind")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind.ends_with("_controller_proxy"))
}

fn transport_proxy_runner_id(record: &AgentTaskRunRecord) -> Option<String> {
    record.runner_id().map(str::to_string).or_else(|| {
        record
            .metadata
            .get("runner_execution_record")
            .and_then(|value| {
                serde_json::from_value::<crate::runner_execution_envelope::RunnerExecutionRecord>(
                    value.clone(),
                )
                .ok()
            })
            .map(|execution| execution.runner_id)
            .filter(|runner_id| !runner_id.trim().is_empty())
    })
}

fn transport_proxy_runner_job_id(record: &AgentTaskRunRecord) -> Option<String> {
    record.runner_job_id().map(str::to_string).or_else(|| {
        record
            .metadata
            .get("runner_execution_record")
            .and_then(|value| {
                serde_json::from_value::<crate::runner_execution_envelope::RunnerExecutionRecord>(
                    value.clone(),
                )
                .ok()
            })
            .and_then(|execution| execution.job_id)
            .filter(|job_id| !job_id.trim().is_empty())
    })
}

pub(crate) fn reconcile_runner_job_snapshot(
    record: &mut AgentTaskRunRecord,
    snapshot: &crate::runner::RunnerJobLogSnapshot,
) -> Result<()> {
    if is_terminal_run_state(record.state) {
        return Ok(());
    }
    validate_runner_job_snapshot(record, snapshot)?;
    let mut reconciled = record.clone();
    reconciled.record_runner_reachable();
    match snapshot.job.status {
        crate::api_jobs::JobStatus::Queued | crate::api_jobs::JobStatus::Running => {
            reconciled.updated_at = Some(now_timestamp());
            update_lifecycle_heartbeat(&mut reconciled);
            let last_seen_at = reconciled.updated_at.clone();
            let metadata = reconciled.ensure_metadata_object();
            metadata.insert("runner_job_status".to_string(), json!(snapshot.job.status));
            metadata.insert("runner_job_last_seen_at".to_string(), json!(last_seen_at));
            metadata.insert("runner_job_events".to_string(), json!(snapshot.events));
            metadata.insert("phase".to_string(), json!("executing"));
            metadata.insert(
                "phase_activity".to_string(),
                json!("provider/executor process is active"),
            );
            metadata.insert("provider_state".to_string(), json!("active"));
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
        crate::api_jobs::JobStatus::Succeeded
        | crate::api_jobs::JobStatus::Failed
        | crate::api_jobs::JobStatus::Cancelled => {
            if let Some(event) = crate::runner::agent_task_lifecycle_event::agent_task_run_plan_lifecycle_event_from_job_events(Some(&snapshot.events)) {
                validate_terminal_child_identity(&reconciled, snapshot, &event)?;
                let projection_plan = aggregate_projection_plan_from_outcomes(&event.aggregate);
                let aggregate_path = store::aggregate_path(&reconciled.run_id)
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|_| "aggregate.json".to_string());
                apply_aggregate_to_record(&mut reconciled, &projection_plan, &event.aggregate, aggregate_path);
                // The aggregate is the task result. A successful enclosing daemon
                // job only proves transport completion, not task success.
                record_runner_job_terminal_metadata(&mut reconciled, snapshot.job.status, &snapshot.events);
                store::write_aggregate_and_record(&reconciled, &event.aggregate)?;
                crate::agent_task_lifecycle::record_terminal_artifact_projection(
                    &mut reconciled,
                    &event.aggregate,
                )?;
            } else {
                apply_runner_job_terminal_state(&mut reconciled, snapshot.job.status, &snapshot.events);
                store::write_record(&reconciled)?;
            }
        }
    }
    *record = reconciled;
    Ok(())
}

fn merge_live_provider_handles(
    record: &mut AgentTaskRunRecord,
    events: &[crate::api_jobs::JobEvent],
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
    snapshot: &crate::runner::RunnerJobLogSnapshot,
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
    snapshot: &crate::runner::RunnerJobLogSnapshot,
    event: &crate::runner::agent_task_lifecycle_event::AgentTaskRunPlanLifecycleEvent,
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

fn is_terminal_run_state(state: AgentTaskRunState) -> bool {
    matches!(
        state,
        AgentTaskRunState::Succeeded
            | AgentTaskRunState::CandidateRecoverable
            | AgentTaskRunState::PartialRecoverable
            | AgentTaskRunState::PartialFailure
            | AgentTaskRunState::Failed
            | AgentTaskRunState::Cancelled
    )
}

fn aggregate_projection_plan(
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

pub(crate) fn apply_runner_job_terminal_state(
    record: &mut AgentTaskRunRecord,
    status: crate::api_jobs::JobStatus,
    events: &[crate::api_jobs::JobEvent],
) {
    let (run_state, task_state) = match status {
        crate::api_jobs::JobStatus::Succeeded => {
            (AgentTaskRunState::Succeeded, AgentTaskState::Succeeded)
        }
        crate::api_jobs::JobStatus::Cancelled => {
            (AgentTaskRunState::Cancelled, AgentTaskState::Cancelled)
        }
        crate::api_jobs::JobStatus::Failed => (AgentTaskRunState::Failed, AgentTaskState::Failed),
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
                crate::runner_execution_envelope::RunnerExecutionRecord::terminal(
                    &runner_job_id,
                    &runner_id,
                    "daemon",
                    if status == crate::api_jobs::JobStatus::Succeeded {
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
    metadata.remove("stale_running");
    metadata.remove("stale_running_reason");
}

fn record_runner_job_terminal_metadata(
    record: &mut AgentTaskRunRecord,
    status: crate::api_jobs::JobStatus,
    events: &[crate::api_jobs::JobEvent],
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
                crate::runner_execution_envelope::RunnerExecutionRecord::terminal(
                    &runner_job_id,
                    &runner_id,
                    "daemon",
                    if status == crate::api_jobs::JobStatus::Succeeded {
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

pub fn run_status(run_id: &str, since_cursor: Option<u64>) -> Result<AgentTaskRunStatus> {
    let record = status(run_id)?;
    let (events, artifact_refs) = match store::read_aggregate(&record.run_id) {
        Ok(aggregate) => {
            let refs = artifact_refs_for_outcomes(&aggregate.outcomes);
            (aggregate.events, refs)
        }
        Err(_) => (queued_events(&record.tasks), record.artifact_refs.clone()),
    };
    let normalized_events = normalize_progress_events(&record.run_id, &events, &artifact_refs);
    let latest_event_cursor = normalized_events
        .last()
        .map(|event| event.sequence)
        .unwrap_or(0);
    let cursor = since_cursor.unwrap_or(0);
    let normalized_events = normalized_events
        .into_iter()
        .filter(|event| event.sequence > cursor)
        .collect();

    Ok(AgentTaskRunStatus {
        schema: schemas::RUN_STATUS.to_string(),
        run_id: record.run_id,
        plan_id: record.plan_id,
        state: record.state,
        submitted_at: record.submitted_at,
        updated_at: record.updated_at,
        totals: record
            .totals
            .unwrap_or_else(|| totals_for_tasks(&record.tasks)),
        latest_event_cursor,
        artifact_refs: record.artifact_refs,
        normalized_events,
    })
}

pub fn list_records() -> Result<Vec<AgentTaskRunRecord>> {
    let mut records = Vec::new();
    for record in store::read_records()? {
        match status(&record.run_id) {
            Ok(record) => records.push(record),
            Err(error) => eprintln!(
                "Warning: skipping malformed agent-task run status for {}: {}",
                record.run_id, error.message
            ),
        }
    }
    records.sort_by(|left, right| {
        right
            .updated_at
            .as_ref()
            .unwrap_or(&right.submitted_at)
            .cmp(left.updated_at.as_ref().unwrap_or(&left.submitted_at))
            .then_with(|| right.submitted_at.cmp(&left.submitted_at))
            .then_with(|| right.run_id.cmp(&left.run_id))
    });
    Ok(records)
}

/// Resolve an aggregate artifact back to its controller-owned durable run.
/// Aggregate paths are passed to promotion commands after the controller has
/// finished, so the path rather than a transient process-local identifier is
/// the durable source identity.
pub fn run_id_for_aggregate_path(path: &std::path::Path) -> Result<Option<String>> {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut matching_run_ids = store::read_records()?
        .into_iter()
        .filter_map(|record| {
            let aggregate_path = store::aggregate_path(&record.run_id).ok()?;
            let aggregate_path = aggregate_path.canonicalize().unwrap_or(aggregate_path);
            (aggregate_path == path).then_some(record.run_id)
        })
        .collect::<Vec<_>>();
    matching_run_ids.sort();
    matching_run_ids.dedup();
    match matching_run_ids.as_slice() {
        [] => Ok(None),
        [run_id] => Ok(Some(run_id.clone())),
        _ => Err(Error::validation_invalid_argument(
            "source",
            "aggregate path is associated with multiple durable agent-task runs",
            Some(path.display().to_string()),
            None,
        )),
    }
}

pub fn run_record_exists(run_id: &str) -> Result<bool> {
    store::record_exists(&sanitize_run_id(run_id))
}

#[derive(Debug, Clone)]
pub struct DetachedLabRunRecord<'a> {
    pub run_id: &'a str,
    pub runner_id: &'a str,
    pub runner_job_id: &'a str,
    pub remote_workspace: &'a str,
    pub remote_command: &'a [String],
}

#[derive(Debug, Clone)]
pub struct LabOffloadProxyPlan<'a> {
    pub run_id: &'a str,
    pub runner_id: &'a str,
    pub remote_workspace: &'a str,
    pub remote_command: &'a [String],
    /// The user task plan, materialized on the controller before the temporary
    /// runner handoff is recorded.
    pub durable_plan: Option<&'a AgentTaskPlan>,
}

/// Persist the controller-owned parent before handing an agent-task workload to
/// a Lab. The runner owns child execution; this record owns the stable local
/// identity and is reconciled from that child once it is accepted.
pub fn record_lab_offload_planned(input: LabOffloadProxyPlan<'_>) -> Result<AgentTaskRunRecord> {
    record_lab_offload_proxy(
        &input.run_id,
        input.runner_id,
        input.remote_workspace,
        input.remote_command,
        input.durable_plan,
    )
}

/// Persist controller-owned setup progress before a runner job exists.
pub fn record_lab_offload_phase(
    requested_run_id: &str,
    runner_id: &str,
    phase: &str,
    remote_workspace: Option<&str>,
    source_checkout: Option<&Value>,
    provider_rotation: Option<&Value>,
    durable_plan: Option<&AgentTaskPlan>,
) -> Result<AgentTaskRunRecord> {
    let placeholder_workspace = remote_workspace.unwrap_or("pending");
    let mut record = record_lab_offload_proxy(
        requested_run_id,
        runner_id,
        placeholder_workspace,
        &[],
        durable_plan,
    )?;
    record.updated_at = Some(now_timestamp());
    let phase_started_at = record.updated_at.clone().unwrap_or_else(now_timestamp);
    let metadata = record.ensure_metadata_object();
    record_lab_offload_phase_metadata(metadata, phase, &phase_started_at);
    metadata.insert("provider_state".to_string(), json!("pending"));
    if let Some(remote_workspace) = remote_workspace {
        metadata.insert("remote_workspace".to_string(), json!(remote_workspace));
    }
    if let Some(source_checkout) = source_checkout {
        metadata.insert("source_checkout".to_string(), source_checkout.clone());
    }
    if let Some(provider_rotation) = provider_rotation {
        metadata.insert("provider_rotation".to_string(), provider_rotation.clone());
    }
    store::write_record(&record)?;
    Ok(record)
}

/// Record child setup executions against the controller proxy. A staging job
/// can outlive the foreground caller, so its runner IDs belong to the durable
/// phase record rather than only transient command output.
pub fn record_lab_offload_phase_executions(
    run_id: &str,
    phase: &str,
    execution_ids: impl IntoIterator<Item = String>,
) -> Result<AgentTaskRunRecord> {
    let mut record = store::read_record(&sanitize_run_id(run_id))?;
    let execution_ids: Vec<String> = execution_ids
        .into_iter()
        .filter(|id| !id.trim().is_empty())
        .collect();
    record.updated_at = Some(now_timestamp());
    let phase_started_at = record.updated_at.clone().unwrap_or_else(now_timestamp);
    let metadata = record.ensure_metadata_object();
    record_lab_offload_phase_metadata(metadata, phase, &phase_started_at);
    metadata.insert(
        "materialization_execution_ids".to_string(),
        json!(execution_ids),
    );
    metadata.insert(
        "materialization_resume".to_string(),
        json!("resume reuses the controller proxy and recorded completed staging"),
    );
    store::write_record(&record)?;
    Ok(record)
}

fn record_lab_offload_phase_metadata(
    metadata: &mut serde_json::Map<String, Value>,
    phase: &str,
    started_at: &str,
) {
    let previous_phase = metadata
        .get("phase")
        .and_then(Value::as_str)
        .map(str::to_string);
    if previous_phase.as_deref() != Some(phase) {
        if let Some(previous_phase) = previous_phase {
            if let Some(entry) = metadata
                .get_mut("phase_history")
                .and_then(Value::as_array_mut)
                .and_then(|entries| {
                    entries.iter_mut().rev().find(|entry| {
                        entry.get("phase").and_then(Value::as_str) == Some(previous_phase.as_str())
                            && entry.get("ended_at").is_none()
                    })
                })
            {
                entry["ended_at"] = json!(started_at);
            }
        }
        metadata
            .entry("phase_history".to_string())
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .expect("phase history is an array")
            .push(json!({ "phase": phase, "started_at": started_at }));
    }
    metadata.insert("phase".to_string(), json!(phase));
    metadata.insert(
        "phase_activity".to_string(),
        json!(format!("Homeboy {phase}")),
    );
}

pub fn record_detached_lab_run(input: DetachedLabRunRecord<'_>) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(input.run_id);
    let plan = detached_lab_plan(&run_id, &input);
    let mut record = match store::read_record(&run_id) {
        Ok(record) => record,
        Err(error)
            if error.code == ErrorCode::InternalJsonError
                && store::record_lacks_typed_metadata(&run_id)? =>
        {
            submit_plan(&plan, Some(&run_id))?
        }
        Err(error) if error.code == ErrorCode::ValidationInvalidArgument => {
            submit_plan(&plan, Some(&run_id))?
        }
        Err(error) => return Err(error),
    };
    if matches!(
        record.state,
        AgentTaskRunState::Succeeded
            | AgentTaskRunState::PartialRecoverable
            | AgentTaskRunState::PartialFailure
            | AgentTaskRunState::Failed
            | AgentTaskRunState::Cancelled
    ) {
        // A terminal proxy must not be resurrected. A later runner job may
        // attach finalized evidence, but only from the original Lab runner.
        if record.runner_id() == Some(input.runner_id) {
            return Ok(record);
        }
        return Err(Error::validation_invalid_argument(
            "run_id",
            format!("agent-task run '{}' is already terminal", record.run_id),
            Some(record.run_id),
            None,
        ));
    }
    if let Err(error) = store::read_plan_path(&record.plan_path) {
        fail_missing_lab_attempt_plan(&mut record, &error)?;
        return Err(Error::internal_io(
            format!(
                "cannot bind Lab runner job because durable attempt plan is unavailable: {}",
                error.message
            ),
            Some(record.plan_path),
        ));
    }
    record.updated_at = Some(now_timestamp());
    set_run_state(&mut record, AgentTaskRunState::Running);
    update_lifecycle_heartbeat(&mut record);
    for task in &mut record.tasks {
        if task.state == AgentTaskState::Queued {
            task.state = AgentTaskState::Running;
        }
    }
    let metadata = record.ensure_metadata_object();
    metadata.insert("kind".to_string(), json!("lab_offload_detached_handoff"));
    metadata.insert("phase".to_string(), json!("awaiting_runner_result"));
    metadata.insert(
        "phase_activity".to_string(),
        json!("controller handoff complete; awaiting authoritative runner daemon result"),
    );
    metadata.insert("provider_state".to_string(), json!("active"));
    let source_snapshot = metadata
        .get("source_checkout")
        .cloned()
        .unwrap_or(Value::Null);
    metadata.insert(
        "runner_handoff".to_string(),
        json!({
            "state": "in_flight",
            "authority": "runner_daemon",
            "identity": {
                "run_id": run_id,
                "runner_id": input.runner_id,
                "runner_job_id": input.runner_job_id,
            },
            "source_snapshot": source_snapshot,
            "continuation": {
                "intent": "reconcile_runner_job",
                "on_active": "retain_running",
                "on_terminal": "project_authoritative_daemon_result_once",
            },
        }),
    );
    metadata.insert("runner_id".to_string(), json!(input.runner_id));
    metadata.insert("runner_job_id".to_string(), json!(input.runner_job_id));
    metadata.insert(
        "remote_workspace".to_string(),
        json!(input.remote_workspace),
    );
    metadata.insert("remote_command".to_string(), json!(input.remote_command));
    metadata.insert(
        "runner_execution_record".to_string(),
        serde_json::to_value(
            crate::runner_execution_envelope::RunnerExecutionRecord::in_flight(
                input.runner_job_id,
                input.runner_id,
                "daemon",
            )
            .with_job_id(input.runner_job_id)
            .with_agent_task_run_id(&run_id),
        )
        .unwrap_or(Value::Null),
    );
    metadata.insert("retryable".to_string(), json!(true));
    metadata.remove("stale_running");
    metadata.remove("stale_running_reason");
    store::write_record(&record)?;
    Ok(record)
}

fn record_lab_offload_proxy(
    requested_run_id: &str,
    runner_id: &str,
    remote_workspace: &str,
    remote_command: &[String],
    durable_plan: Option<&AgentTaskPlan>,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(requested_run_id);
    let input = DetachedLabRunRecord {
        run_id: &run_id,
        runner_id,
        // This placeholder is removed immediately below. Keeping construction
        // centralized lets the proxy and bound child share one plan shape.
        runner_job_id: "unbound",
        remote_workspace,
        remote_command,
    };
    let mut plan = detached_lab_plan(&run_id, &input);
    let task = &mut plan.tasks[0];
    if let Some(inputs) = task.inputs.as_object_mut() {
        inputs.remove("runner_job_id");
    }
    task.source_refs.clear();
    if let Some(materialization) = task.workspace.materialization.as_object_mut() {
        materialization.remove("runner_job_id");
    }
    if let Some(metadata) = task.metadata.as_object_mut() {
        metadata.remove("runner_job_id");
    }
    if let Some(metadata) = plan.metadata.as_object_mut() {
        metadata.remove("runner_job_id");
    }
    let mut record = match store::read_record(&run_id) {
        Ok(record) => record,
        Err(error)
            if error.code == ErrorCode::InternalJsonError
                && store::record_lacks_typed_metadata(&run_id)? =>
        {
            submit_plan(durable_plan.unwrap_or(&plan), Some(&run_id))?
        }
        Err(error) if error.code == ErrorCode::ValidationInvalidArgument => {
            submit_plan(durable_plan.unwrap_or(&plan), Some(&run_id))?
        }
        Err(error) => return Err(error),
    };
    // A previous interruption may have committed the record but not its plan.
    // Repair from the controller-compiled plan before exposing another handoff
    // phase; without it the runner would later create a fake running attempt.
    if store::read_plan_path(&record.plan_path).is_err() {
        if let Some(durable_plan) = durable_plan {
            let plan_path = store::write_plan(&run_id, durable_plan)?;
            record.plan_path = plan_path.display().to_string();
        } else {
            let error = Error::internal_io(
                "durable attempt plan is unavailable during Lab handoff recovery",
                Some(record.plan_path.clone()),
            );
            fail_missing_lab_attempt_plan(&mut record, &error)?;
            return Err(error);
        }
    }
    let metadata = record.ensure_metadata_object();
    metadata.insert("kind".to_string(), json!("lab_offload_controller_proxy"));
    metadata.insert("runner_id".to_string(), json!(runner_id));
    if remote_workspace != "pending" {
        metadata.insert("remote_workspace".to_string(), json!(remote_workspace));
    }
    if !remote_command.is_empty() {
        metadata.insert("remote_command".to_string(), json!(remote_command));
    }
    metadata.insert("retryable".to_string(), json!(true));
    metadata.insert(
        "runner_execution_record".to_string(),
        serde_json::to_value(
            crate::runner_execution_envelope::RunnerExecutionRecord::planned(
                &run_id, runner_id, "daemon",
            )
            .with_agent_task_run_id(&run_id),
        )
        .unwrap_or(Value::Null),
    );
    store::write_record(&record)?;
    Ok(record)
}

fn fail_missing_lab_attempt_plan(record: &mut AgentTaskRunRecord, error: &Error) -> Result<()> {
    record.updated_at = Some(now_timestamp());
    set_run_state(record, AgentTaskRunState::Failed);
    for task in &mut record.tasks {
        if matches!(task.state, AgentTaskState::Queued | AgentTaskState::Running) {
            task.state = AgentTaskState::Failed;
        }
    }
    let metadata = record.ensure_metadata_object();
    metadata.insert(
        "pre_execution_failure".to_string(),
        json!({
            "phase": "lab_attempt_plan_recovery",
            "error": error.message,
        }),
    );
    metadata.insert("retryable".to_string(), json!(true));
    store::write_record(record)
}

fn detached_lab_plan(run_id: &str, input: &DetachedLabRunRecord<'_>) -> AgentTaskPlan {
    let task = AgentTaskRequest {
        schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
        task_id: format!("{run_id}-lab-handoff"),
        group_key: Some("lab-offload".to_string()),
        parent_plan_id: None,
        executor: AgentTaskExecutor {
            backend: "homeboy-lab".to_string(),
            selector: Some(input.runner_id.to_string()),
            runtime_selection: None,
            required_capabilities: Vec::new(),
            secret_env: Vec::new(),
            model: None,
            config: Value::Null,
        },
        instructions: "Detached Lab agent-task run handed off to a durable runner job.".to_string(),
        inputs: json!({
            "runner_id": input.runner_id,
            "runner_job_id": input.runner_job_id,
            "remote_workspace": input.remote_workspace,
            "remote_command": input.remote_command,
        }),
        source_refs: vec![AgentTaskSourceRef {
            kind: "lab-offload-runner-job".to_string(),
            uri: format!(
                "homeboy://runner/{}/job/{}",
                input.runner_id, input.runner_job_id
            ),
            revision: None,
        }],
        workspace: AgentTaskWorkspace {
            mode: AgentTaskWorkspaceMode::Existing,
            root: Some(input.remote_workspace.to_string()),
            kind: Some("lab-offload".to_string()),
            cleanup: Some("preserve".to_string()),
            materialization: json!({
                "runner_id": input.runner_id,
                "runner_job_id": input.runner_job_id,
            }),
            ..AgentTaskWorkspace::default()
        },
        component_contracts: Vec::new(),
        policy: AgentTaskPolicy::default(),
        limits: AgentTaskLimits::default(),
        expected_artifacts: Vec::new(),
        artifact_declarations: Vec::new(),
        metadata: json!({
            "kind": "lab_offload_detached_handoff",
            "runner_id": input.runner_id,
            "runner_job_id": input.runner_job_id,
        }),
    };
    let mut plan = AgentTaskPlan::new(format!("{run_id}-lab-offload"), vec![task]);
    plan.group_key = Some("lab-offload".to_string());
    plan.metadata = json!({
        "kind": "lab_offload_detached_handoff",
        "runner_id": input.runner_id,
        "runner_job_id": input.runner_job_id,
        "remote_workspace": input.remote_workspace,
    });
    plan
}

pub fn mark_resuming(run_id: &str) -> Result<AgentTaskRunRecord> {
    let mut record = store::read_record(&sanitize_run_id(run_id))?;
    if matches!(
        record.state,
        AgentTaskRunState::Succeeded
            | AgentTaskRunState::PartialRecoverable
            | AgentTaskRunState::PartialFailure
            | AgentTaskRunState::Failed
            | AgentTaskRunState::Cancelled
    ) {
        return Err(Error::validation_invalid_argument(
            "run_id",
            format!(
                "agent-task run '{}' is already terminal with state {:?}",
                record.run_id, record.state
            ),
            Some(record.run_id),
            None,
        ));
    }

    let metadata = record.ensure_metadata_object();
    metadata.insert("resume_requested_at".to_string(), json!(now_timestamp()));
    store::write_record(&record)?;
    mark_running(run_id)
}

pub fn retry(run_id: &str, requested_run_id: Option<&str>) -> Result<AgentTaskRunRecord> {
    let source = store::read_record(&resolve_run_id(run_id)?)?;
    let plan = store::read_plan_path(&source.plan_path)?;
    let mut retry = submit_plan(&plan, requested_run_id)?;
    let metadata = retry.ensure_metadata_object();
    if let Some(route) =
        crate::notification_route::NotificationRoute::from_metadata(&source.metadata)
    {
        // Retries are new durable runs, but retain the initiating route. Resume
        // operates on the same record and therefore needs no copy.
        metadata.insert(
            crate::notification_route::NOTIFICATION_ROUTE_METADATA_KEY.to_string(),
            serde_json::to_value(route).expect("notification route is serializable"),
        );
    }
    let retry_origin = [
        "runner_id",
        "runner_job_id",
        "remote_workspace",
        "remote_command",
        "runner_execution_record",
        "pre_execution_failure",
        "runner_job_events",
    ]
    .into_iter()
    .filter_map(|key| {
        source
            .metadata
            .get(key)
            .map(|value| (key.to_string(), value.clone()))
    })
    .collect::<serde_json::Map<_, _>>();
    if !retry_origin.is_empty() {
        metadata.insert("retry_origin".to_string(), Value::Object(retry_origin));
    }
    metadata.insert("retry_of".to_string(), json!(source.run_id));
    metadata.insert("retry_requested_at".to_string(), json!(now_timestamp()));
    store::write_record(&retry)?;
    Ok(retry)
}

pub fn logs(run_id: &str) -> Result<AgentTaskRunLog> {
    // Status reconciliation fetches the live daemon snapshot for a bound Lab
    // child, making executor progress visible before the child is terminal.
    let record = status(run_id)?;
    let run_id = record.run_id.clone();
    let (events, artifact_refs) = match store::read_aggregate(&run_id) {
        Ok(aggregate) => {
            let refs = artifact_refs_for_outcomes(&aggregate.outcomes);
            (aggregate.events, refs)
        }
        Err(_) => (
            runner_job_progress_events(&record).unwrap_or_else(|| queued_events(&record.tasks)),
            record.artifact_refs.clone(),
        ),
    };
    let normalized_events = normalize_progress_events(&run_id, &events, &artifact_refs);
    Ok(AgentTaskRunLog {
        schema: schemas::RUN_LOG.to_string(),
        run_id,
        events,
        normalized_events,
    })
}

fn runner_job_progress_events(record: &AgentTaskRunRecord) -> Option<Vec<AgentTaskProgressEvent>> {
    let events = record.metadata.get("runner_job_events")?.as_array()?;
    let task_id = record
        .tasks
        .first()
        .map(|task| task.task_id.clone())
        .unwrap_or_else(|| record.run_id.clone());
    Some(
        events
            .iter()
            .map(|event| AgentTaskProgressEvent {
                task_id: task_id.clone(),
                state: AgentTaskState::Running,
                attempt: 0,
                message: serde_json::to_string(event).ok(),
            })
            .collect(),
    )
}

pub fn artifacts(run_id: &str) -> Result<AgentTaskRunArtifacts> {
    let record = status(run_id)?;
    let run_id = record.run_id.clone();
    let aggregate = store::read_aggregate(&run_id).ok();
    let latest_executor_evidence = record.latest_executor_evidence.as_ref();
    Ok(AgentTaskRunArtifacts {
        schema: schemas::RUN_ARTIFACTS.to_string(),
        run_id,
        artifacts: aggregate
            .as_ref()
            .map(crate::agent_task_artifacts::reviewer_facing_aggregate)
            .map(|aggregate| aggregate_artifacts(Some(&aggregate)))
            .unwrap_or_default(),
        evidence_refs: aggregate_evidence_refs(aggregate.as_ref(), latest_executor_evidence),
    })
}

/// Read the aggregate after a transport reconciliation completed it without
/// scheduling the controller-side synthetic handoff task.
pub fn read_aggregate(run_id: &str) -> Result<AgentTaskAggregate> {
    let run_id = resolve_run_id(run_id)?;
    store::read_aggregate(&run_id)
}

pub fn aggregate_source(run_id: &str) -> Result<(String, PathBuf)> {
    let record = status(run_id)?;
    record.aggregate_path.as_ref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "run_id",
            format!(
                "agent-task run '{}' has no aggregate artifact yet",
                record.run_id
            ),
            Some(record.run_id.clone()),
            None,
        )
    })?;
    let aggregate = store::read_aggregate(&record.run_id)?;
    let raw = serde_json::to_string_pretty(&aggregate).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some(format!("serialize agent-task aggregate {}", record.run_id)),
        )
    })?;
    let path = store::aggregate_path(&record.run_id)?;
    Ok((raw, path))
}

pub fn record_cook_attempt(
    cook_id: &str,
    attempt: u32,
    run_id: &str,
) -> Result<AgentTaskCookIndex> {
    let mut record = store::read_record(&sanitize_run_id(run_id))?;
    let recorded_at = now_timestamp();
    let metadata = record.ensure_metadata_object();
    metadata.insert("cook_id".to_string(), json!(sanitize_run_id(cook_id)));
    metadata.insert("cook_attempt".to_string(), json!(attempt));
    store::write_record(&record)?;
    store::write_cook_index_attempt(cook_id, attempt, run_id, recorded_at)
}

pub fn cook_index(cook_id: &str) -> Result<AgentTaskCookIndex> {
    store::read_cook_index(&sanitize_run_id(cook_id))
}

fn resolve_run_id(run_id: &str) -> Result<String> {
    let run_id = sanitize_run_id(run_id);
    match store::read_cook_index(&run_id) {
        Ok(index) => Ok(index.latest_run_id),
        Err(_) => Ok(run_id),
    }
}

pub fn record_promotion(run_id: &str, promotion: Value) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let record = store::mutate_record(&run_id, |record| {
        if record.metadata.get("latest_promotion") == Some(&promotion) {
            return false;
        }
        record.updated_at = Some(now_timestamp());
        let metadata = record.ensure_metadata_object();
        let promotions = metadata
            .entry("promotions".to_string())
            .or_insert_with(|| json!([]));
        if !promotions.is_array() {
            *promotions = json!([]);
        }
        promotions
            .as_array_mut()
            .expect("promotions array")
            .push(promotion.clone());
        metadata.insert("latest_promotion".to_string(), promotion);
        true
    })?;
    match record {
        Some(record) => Ok(record),
        None => store::read_record(&run_id),
    }
}
