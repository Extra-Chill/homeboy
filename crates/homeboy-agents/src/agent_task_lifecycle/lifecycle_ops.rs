use super::*;
use homeboy_core::api_jobs::RemoteRunnerJobRequest;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};

const LAB_HANDOFF_ACCEPTANCE_TIMEOUT_SECONDS: i64 = 120;
pub(crate) const EXPIRED_LAB_HANDOFF_REASON: &str =
    "runner handoff acceptance deadline expired before a runner job was recorded";

fn lab_handoff_acceptance_timeout_seconds() -> i64 {
    std::env::var("HOMEBOY_TEST_LAB_HANDOFF_ACCEPTANCE_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|seconds| *seconds >= 0)
        .unwrap_or(LAB_HANDOFF_ACCEPTANCE_TIMEOUT_SECONDS)
}

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

    let plan = store::read_controller_plan(&run_id)?;
    aggregate.status = aggregate_status(&aggregate.outcomes);
    aggregate.totals = aggregate_totals(plan.tasks.len(), &aggregate.outcomes);
    let aggregate_path = store::aggregate_path(&run_id)?.display().to_string();
    apply_aggregate_to_record(&mut record, &plan, &aggregate, aggregate_path);
    store::write_aggregate_and_record(&record, &aggregate)?;
    record_terminal_artifact_projection(&mut record, &aggregate)?;
    Ok(true)
}

fn candidate_artifact_url(run_id: &str, task_id: &str, artifact_id: &str) -> String {
    use homeboy_core::execution_contract::encode_uri_component;

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

struct LabHandoffLock {
    // Retains the advisory lock until acceptance or expiry has been persisted.
    #[allow(dead_code)]
    file: File,
}

impl LabHandoffLock {
    fn lock(run_id: &str) -> Result<Self> {
        let lock_path = paths::homeboy_data()?
            .join("agent-task-runs")
            .join(run_id)
            .join("lab-handoff.lock");
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| Error::internal_io(error.to_string(), None))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|error| {
                Error::internal_io(error.to_string(), Some("open Lab handoff lock".to_string()))
            })?;
        #[cfg(unix)]
        if unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&file), libc::LOCK_EX) } != 0 {
            return Err(Error::internal_io(
                std::io::Error::last_os_error().to_string(),
                Some("lock Lab handoff".to_string()),
            ));
        }
        Ok(Self { file })
    }
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
    submit_plan_with_runtime_admission(plan, requested_run_id, |run_id| {
        homeboy_core::controller_runtime::admit_current_for_with_cancellation_check(run_id, || {
            Ok(store::read_record(run_id)?.state.is_terminal())
        })
    })
}

pub(crate) trait RuntimeAdmissionEvidence {
    fn runtime(&self) -> Value;
}

impl RuntimeAdmissionEvidence for homeboy_core::controller_runtime::RuntimeAdmission {
    fn runtime(&self) -> Value {
        self.runtime.clone()
    }
}

#[cfg(test)]
impl RuntimeAdmissionEvidence for Value {
    fn runtime(&self) -> Value {
        self.clone()
    }
}

/// Persist the run identity before controller admission so an admission failure
/// remains inspectable and retryable through the normal lifecycle commands.
pub(crate) fn submit_plan_with_runtime_admission<F, A>(
    plan: &AgentTaskPlan,
    requested_run_id: Option<&str>,
    admit_runtime: F,
) -> Result<AgentTaskRunRecord>
where
    F: FnOnce(&str) -> Result<A>,
    A: RuntimeAdmissionEvidence,
{
    let run_id = requested_run_id
        .map(sanitize_run_id)
        .unwrap_or_else(default_run_id);
    let plan_path = store::write_plan(&run_id, plan)?;

    let mut metadata = json!({
        "task_count": plan.tasks.len(),
        "max_concurrency": plan.options.max_concurrency,
        "provider_run_ids": [],
        "provider_executions_consumed": 0,
        "controller_identity": homeboy_core::build_identity::current().display,
        "lifecycle_schema": RUN_LIFECYCLE_RECORD_SCHEMA,
        "note": "submitted tasks are durable; provider run ids are recorded after an executor returns them as generic artifacts or evidence refs"
    });
    let execution_runner_id = execution_runner_id();
    if let Some(runner_id) = execution_runner_id.as_deref() {
        metadata["runner_id"] = json!(runner_id);
    }
    if let Some(route) = homeboy_core::notification_route::current() {
        route.insert_into_metadata(&mut metadata);
    }

    let mut record = AgentTaskRunRecord {
        schema: schemas::RUN.to_string(),
        run_id: run_id.clone(),
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
        lab_handoff: None,
        candidate_adoption: None,
        adoption_run_id: None,
        metadata,
    };
    if let Ok(existing) = store::read_record(&run_id) {
        if execution_runner_id.as_deref() == existing.runner_id() {
            // A foreground daemon binds its job before launching runner-local
            // `run-plan`. Keep that transport identity when run-plan replaces
            // the staged record, or terminal projection cannot join its daemon
            // snapshot back to the completed agent-task run.
            if let Some(runner_job_id) = existing.runner_job_id() {
                record.metadata["runner_job_id"] = json!(runner_job_id);
            }
            if existing.lab_handoff.as_ref().is_some_and(|handoff| {
                handoff.state == AgentTaskLabHandoffState::Accepted
                    && handoff.authority == AgentTaskLabHandoffAuthority::RunnerDaemon
            }) {
                record.lab_handoff = existing.lab_handoff;
            }
        }
    }
    store::write_record(&record)?;

    // The queue is durable independently of this foreground controller. Status
    // and cancellation can therefore resolve a waiter after a restart.
    if let Ok(admission) = homeboy_core::controller_runtime::admission_status(&run_id) {
        record.metadata["controller_admission"] = admission;
        store::write_record(&record)?;
    }

    match admit_runtime(&run_id) {
        Ok(admission) => {
            // The admission claim checks this state under the queue lock. Read
            // it once more before recording runtime provenance or dispatching
            // any provider work in case cancellation won immediately after.
            if let Ok(cancelled) = store::read_record(&run_id) {
                if cancelled.state.is_terminal() {
                    return Ok(cancelled);
                }
            }
            record.metadata[homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY] =
                admission.runtime();
            store::write_record(&record)?;
        }
        Err(error) => {
            // Cancellation is persisted before removing a queue entry. Do not
            // overwrite that terminal lifecycle state with a synthetic
            // pre-execution admission failure when the waiter wakes up.
            if let Ok(cancelled) = store::read_record(&run_id) {
                if cancelled.state == AgentTaskRunState::Cancelled
                    || cancelled.metadata["controller_admission_cancellation_requested"] == true
                {
                    return Ok(cancelled);
                }
            }
            record_pre_execution_failure(&run_id, plan, "controller_admission", &error)?;
            return Err(error);
        }
    }
    Ok(record)
}

pub(crate) fn execution_runner_id() -> Option<String> {
    std::env::var(homeboy_core::lab_contract::LAB_EXECUTION_RUNNER_ID_ENV)
        .ok()
        .filter(|runner_id| !runner_id.trim().is_empty())
}

/// Bind an inherited route when a detached workload recreates an agent-task run.
pub fn persist_notification_route(
    run_id: &str,
    route: &homeboy_core::notification_route::NotificationRoute,
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
    let run_id = resolve_run_id(run_id)?;
    store::read_controller_plan(&run_id)
}

/// Load the plan owned by this controller's durable run identity. Runner paths
/// projected into lifecycle metadata are transport evidence, not retry input.
pub fn load_controller_plan(run_id: &str) -> Result<AgentTaskPlan> {
    let run_id = resolve_run_id(run_id)?;
    store::read_controller_plan(&run_id)
}

/// Load a durable plan for a scheduler or provider execution. This is the only
/// read path allowed to upgrade a legacy execution-budget envelope.
pub fn load_plan_for_execution(run_id: &str) -> Result<AgentTaskPlan> {
    let run_id = resolve_run_id(run_id)?;
    store::read_controller_plan_for_execution(&run_id)
}

/// Validate a queued lifecycle's pinned controller without scheduling provider work.
pub fn validate_controller_runtime(run_id: &str) -> Result<AgentTaskRunRecord> {
    let mut record = store::read_record(&sanitize_run_id(run_id))?;
    migrate_record_controller_runtime(&mut record)?;
    homeboy_core::controller_runtime::validate(
        record
            .metadata
            .get(homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY)
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "controller_runtime",
                    "durable run has no controller runtime pin",
                    Some(record.run_id.clone()),
                    None,
                )
            })?,
    )?;
    Ok(record)
}

/// Resolve the compatible immutable executable for a lifecycle mutation.
/// Legacy pins are migrated atomically before returning a path for re-exec.
pub fn pinned_runtime_for_mutation(run_id: &str) -> Result<Option<std::path::PathBuf>> {
    let mut record = store::read_record(&resolve_run_id(run_id)?)?;
    migrate_record_controller_runtime(&mut record)?;
    homeboy_core::controller_runtime::pinned_executable_for_mutation(
        &record.metadata,
        &homeboy_core::build_identity::current().display,
    )
}

/// Seal the currently executing controller into an immutable runtime before a
/// new cook begins its local routing and admission work.
pub fn pin_current_controller_runtime() -> Result<std::path::PathBuf> {
    let runtime = homeboy_core::controller_runtime::pin_current()?;
    runtime
        .pointer("/originating/pinned_executable")
        .and_then(Value::as_str)
        .map(std::path::PathBuf::from)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "controller_runtime",
                "new controller runtime pin has no immutable executable",
                None,
                None,
            )
        })
}

/// Prune immutable controller pins through the durable lifecycle ownership
/// boundary so nonterminal records remain authoritative retention roots.
pub fn prune_controller_runtime_pins(
    apply: bool,
) -> Result<homeboy_core::controller_runtime::ControllerRuntimePruneResult> {
    homeboy_core::controller_runtime::prune_pins(apply)
}

fn migrate_record_controller_runtime(record: &mut AgentTaskRunRecord) -> Result<()> {
    let Some(runtime) = record
        .metadata
        .get(homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY)
    else {
        return Ok(());
    };
    let original = runtime.clone();
    let migrated =
        homeboy_core::controller_runtime::migrate_legacy_pin_and_persist(&original, |migrated| {
            record.metadata[homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY] =
                migrated.clone();
            store::write_record(record)
        })?;
    record.metadata[homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY] = migrated;
    Ok(())
}

/// Repair only the executable artifact named by durable controller provenance.
pub fn recover_controller_runtime(
    run_id: &str,
    artifact: Option<&std::path::Path>,
    source: Option<&std::path::Path>,
) -> Result<Value> {
    let mut record = store::read_record(&sanitize_run_id(run_id))?;
    let runtime = record
        .metadata
        .get(homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "controller_runtime",
                "durable run has no controller runtime pin",
                Some(record.run_id.clone()),
                None,
            )
        })?;
    let runtime = runtime.clone();
    let recovered = homeboy_core::controller_runtime::recover_pin_and_persist(
        &runtime,
        artifact,
        source,
        |recovered| {
            record.metadata[homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY] =
                recovered.clone();
            store::write_record(&record)
        },
    )?;
    Ok(recovered)
}

pub fn mark_running(run_id: &str) -> Result<AgentTaskRunRecord> {
    let mut record = store::read_record(&sanitize_run_id(run_id))?;
    migrate_record_controller_runtime(&mut record)?;
    homeboy_core::controller_runtime::validate_for_mutation(
        &record.metadata,
        &homeboy_core::build_identity::current().display,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderExecutionReservation {
    Acquired,
    AlreadyReserved,
}

/// Durably reserve one provider execution before the scheduler blocks on the
/// backend. A resumed controller must reconcile an existing reservation rather
/// than dispatching the same `(task_id, attempt)` a second time.
pub fn reserve_provider_execution(
    run_id: &str,
    task: &AgentTaskRequest,
    attempt: u32,
) -> Result<ProviderExecutionReservation> {
    let run_id = sanitize_run_id(run_id);
    let execution_key = format!("{}:{attempt}", task.task_id);
    let mut reservation = ProviderExecutionReservation::AlreadyReserved;
    store::mutate_record(&run_id, |record| {
        let metadata = record.ensure_metadata_object();
        let consumed = {
            let executions = metadata
                .entry("provider_executions".to_string())
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .expect("provider_executions must be an array");
            if executions
                .iter()
                .any(|execution| execution["key"] == execution_key)
            {
                return false;
            }
            executions.push(json!({
                "key": execution_key,
                "task_id": task.task_id,
                "attempt": attempt,
                "backend": task.executor.backend,
                "model": task.executor.model(),
                "state": "running",
                "started_at": now_timestamp(),
            }));
            executions.len()
        };
        metadata.insert("provider_executions_consumed".to_string(), json!(consumed));
        reservation = ProviderExecutionReservation::Acquired;
        true
    })?;
    Ok(reservation)
}

/// Record the provider's terminal result before controller-owned patch
/// harvesting. Harvesting can fail or be interrupted independently of the
/// provider execution, so it must not leave this reservation running.
pub fn record_provider_execution_terminal(
    run_id: &str,
    task_id: &str,
    attempt: u32,
    state: &str,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let execution_key = format!("{task_id}:{attempt}");
    let mut found = false;
    let record = store::mutate_record(&run_id, |record| {
        let Some(execution) = record
            .ensure_metadata_object()
            .get_mut("provider_executions")
            .and_then(Value::as_array_mut)
            .and_then(|executions| {
                executions
                    .iter_mut()
                    .find(|execution| execution["key"] == execution_key)
            })
        else {
            return false;
        };
        execution["state"] = json!(state);
        execution["finished_at"] = json!(now_timestamp());
        found = true;
        true
    })?;
    if !found {
        return Err(Error::internal_unexpected(
            "provider execution reached a terminal result without its durable attempt record",
        ));
    }
    record.ok_or_else(|| {
        Error::internal_unexpected("provider execution terminal record was unchanged")
    })
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

/// Reproject terminal artifacts from controller-owned durable state. This is a
/// recovery path for historical runner results whose aggregate was persisted
/// before the controller finalized its artifact-byte projection.
pub fn reconcile_terminal_artifact_projection(run_id: &str) -> Result<bool> {
    let mut record = store::read_record(&sanitize_run_id(run_id))?;
    if !record.state.is_terminal() {
        return Ok(false);
    }

    // Require the controller-owned plan as part of the durable lifecycle
    // contract even though artifact projection derives its byte checks from the
    // aggregate. The runner staging plan is never a recovery input.
    let _plan = store::read_controller_plan(&record.run_id)?;
    let aggregate = store::read_aggregate(&record.run_id)?;
    record_terminal_artifact_projection(&mut record, &aggregate)?;
    Ok(true)
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

pub fn status(run_id: &str) -> Result<AgentTaskRunRecord> {
    let requested_run_id = sanitize_run_id(run_id);
    let resolved_run_id = resolve_run_id(run_id)?;
    let _ = reconcile_deferred_candidate(&resolved_run_id)?;
    let mut record = store::read_record(&resolved_run_id)?;
    if let Ok(admission) = homeboy_core::controller_runtime::admission_status(&record.run_id) {
        record.metadata["controller_admission"] = admission;
        store::write_record(&record)?;
    }
    if reconcile_candidate_adoption(&mut record) {
        store::write_record(&record)?;
    }
    if reconcile_pending_runner_submission_intent(&resolved_run_id)? {
        record = store::read_record(&resolved_run_id)?;
    }
    if has_expired_pending_runner_submission_intent(&record, chrono::Utc::now()) {
        let _ = expire_unaccepted_lab_handoff(&resolved_run_id)?;
        record = store::read_record(&resolved_run_id)?;
    }
    if !record.state.is_terminal() {
        let controller_plan = store::read_controller_plan(&record.run_id)?;
        let controller_plan_path = store::controller_plan_path(&record.run_id)?
            .display()
            .to_string();
        if record.plan_path != controller_plan_path {
            record.plan_path = controller_plan_path;
            store::write_record(&record)?;
        }
        if let Ok(aggregate) = store::read_aggregate(&record.run_id) {
            let aggregate_path = store::aggregate_path(&record.run_id)
                .map(|path| path.display().to_string())
                .unwrap_or_else(|_| "aggregate.json".to_string());
            let mut reconciled = record.clone();
            let projection_plan = aggregate_projection_plan(&controller_plan, &aggregate);
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
    if record.state.is_terminal() {
        if let Ok(aggregate) = store::read_aggregate(&record.run_id) {
            if reconcile_terminal_provider_models(&mut record, &aggregate) {
                store::write_record(&record)?;
            }
            if !crate::agent_task_lifecycle::terminal_artifact_projection_is_verified(
                &record, &aggregate,
            )? {
                crate::agent_task_lifecycle::record_terminal_artifact_projection(
                    &mut record,
                    &aggregate,
                )?;
            }
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
                let existing_scheduler_status = record
                    .metadata
                    .get("cook_continuation_scheduler")
                    .and_then(Value::as_object)
                    .and_then(|scheduler| scheduler.get("status"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                match crate::agent_task_service::enqueue_terminal_continuation(
                    &cook_id,
                    &record.run_id,
                ) {
                    Ok(enqueued) => {
                        let run_id = record.run_id.clone();
                        let candidate = record.latest_executor_evidence.as_ref().map(|evidence| {
                            json!({
                                "task_id": evidence.task_id,
                                "provider_run_id": evidence.provider_run_id,
                                "normalized_output_ref": evidence.normalized_output_ref,
                            })
                        });
                        let status = if enqueued {
                            "queued"
                        } else {
                            existing_scheduler_status
                                .as_deref()
                                .unwrap_or("already_queued_or_completed")
                        };
                        record.ensure_metadata_object().insert(
                            "cook_continuation_scheduler".to_string(),
                            json!({
                                "status": status,
                                "cook_id": cook_id,
                                "run_id": run_id,
                                "candidate": candidate,
                            }),
                        );
                        store::write_record(&record)?;
                    }
                    Err(error) => {
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
    }
    if requested_run_id != record.run_id {
        if let Ok(index) = store::read_cook_index(&requested_run_id) {
            project_cook_alias_adoption(&mut record, &index)?;
            let metadata = record.ensure_metadata_object();
            metadata.insert("cook_alias".to_string(), json!(requested_run_id));
            metadata.insert(
                "cook_index".to_string(),
                serde_json::to_value(index).unwrap_or(Value::Null),
            );
        }
    }
    Ok(record)
}

/// Refresh accepted runner handoffs and expire unbound controller handoffs before
/// a read model (such as activity) projects lifecycle state. A controller wait
/// expiry is not terminal after a runner job is recorded: the runner daemon
/// remains the authority until it reports a terminal job result.
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
    has_complete_pending_runner_submission_intent(record)
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
fn expire_unaccepted_lab_handoff(run_id: &str) -> Result<bool> {
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
            // Discovery health owns malformed-record reporting. A transient
            // status refresh failure must not reintroduce stderr-only state.
            Err(_) => (),
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

pub fn list_records_with_health() -> Result<(Vec<AgentTaskRunRecord>, AgentTaskRecordHealthSummary)>
{
    let (records, health) = read_records_with_health()?;
    let mut refreshed = Vec::new();
    for record in records {
        if let Ok(record) = status(&record.run_id) {
            refreshed.push(record);
        }
    }
    refreshed.sort_by(|left, right| {
        right
            .updated_at
            .as_ref()
            .unwrap_or(&right.submitted_at)
            .cmp(left.updated_at.as_ref().unwrap_or(&left.submitted_at))
            .then_with(|| right.submitted_at.cmp(&left.submitted_at))
            .then_with(|| right.run_id.cmp(&left.run_id))
    });
    Ok((refreshed, health))
}

/// Read the durable registry snapshot without runner reconciliation. Bounded
/// recovery readers use this path so disconnected historical runner mirrors
/// cannot delay access to controller-owned state.
pub fn read_records_with_health() -> Result<(Vec<AgentTaskRunRecord>, AgentTaskRecordHealthSummary)>
{
    let (mut records, health) = store::read_records_with_health()?;
    records.sort_by(|left, right| {
        right
            .updated_at
            .as_ref()
            .unwrap_or(&right.submitted_at)
            .cmp(left.updated_at.as_ref().unwrap_or(&left.submitted_at))
            .then_with(|| right.submitted_at.cmp(&left.submitted_at))
            .then_with(|| right.run_id.cmp(&left.run_id))
    });
    Ok((records, health))
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

/// Whether a durable run record exists for `run_id` after the same resolution
/// `retry` applies (a cook id resolves to its latest run). The plain
/// `run_record_exists` is an exact-match check, so a resolvable id (e.g. a cook
/// id) reports absent even though `retry` would succeed — which previously made
/// the Lab retry handoff silently fall through and ship an unrunnable
/// `agent-task retry <id>` to a runner with no such record (#8390).
pub fn run_record_exists_resolved(run_id: &str) -> Result<bool> {
    store::record_exists(&resolve_run_id(run_id)?)
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
    if record.state.is_terminal() {
        return Ok(record);
    }
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
    if record.state.is_terminal() {
        return Ok(record);
    }
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
    let _lock = LabHandoffLock::lock(&run_id)?;
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
    if let Some(problem) = record.lab_handoff_validation_error() {
        return Err(Error::validation_invalid_argument(
            "lab_handoff",
            problem,
            Some(record.run_id.clone()),
            None,
        ));
    }
    if let Some(accepted) = record.lab_handoff.as_ref().filter(|handoff| {
        handoff.state == AgentTaskLabHandoffState::Accepted
            && handoff.authority == AgentTaskLabHandoffAuthority::RunnerDaemon
    }) {
        if accepted.runner_id == input.runner_id
            && accepted.runner_job_id.as_deref() == Some(input.runner_job_id)
        {
            return Ok(record);
        }
        return Err(Error::validation_invalid_argument(
            "lab_handoff",
            format!(
                "Lab handoff for run '{}' is already accepted by runner '{}' job '{}'; refusing a different acceptance",
                record.run_id,
                accepted.runner_id,
                accepted.runner_job_id.as_deref().unwrap_or_default(),
            ),
            Some(record.run_id.clone()),
            None,
        ));
    }
    if record.lab_handoff.is_none() && record.runner_id().is_some_and(|id| id != input.runner_id) {
        return Err(Error::validation_invalid_argument(
            "runner_id",
            format!(
                "Lab handoff for run '{}' is assigned to runner '{}'; refusing acceptance from '{}'",
                record.run_id,
                record.runner_id().unwrap_or_default(),
                input.runner_id,
            ),
            Some(record.run_id.clone()),
            None,
        ));
    }
    if let Some(pending) = record.lab_handoff.as_ref().filter(|handoff| {
        handoff.state == AgentTaskLabHandoffState::Pending
            && handoff.authority == AgentTaskLabHandoffAuthority::Controller
    }) {
        if pending.runner_id != input.runner_id {
            return Err(Error::validation_invalid_argument(
                "runner_id",
                format!(
                    "Lab handoff for run '{}' is pending acceptance by runner '{}'; refusing acceptance from '{}'",
                    record.run_id, pending.runner_id, input.runner_id,
                ),
                Some(record.run_id.clone()),
                None,
            ));
        }
    }
    let expired_unaccepted_handoff = record.state == AgentTaskRunState::Cancelled
        && record.lab_handoff.as_ref().is_some_and(|handoff| {
            handoff.state == AgentTaskLabHandoffState::Expired
                && handoff.authority == AgentTaskLabHandoffAuthority::Controller
                && handoff.runner_id == input.runner_id
        });
    if !expired_unaccepted_handoff
        && matches!(
            record.state,
            AgentTaskRunState::Succeeded
                | AgentTaskRunState::PartialRecoverable
                | AgentTaskRunState::PartialFailure
                | AgentTaskRunState::Failed
                | AgentTaskRunState::Cancelled
        )
    {
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
    if let Err(error) = store::read_controller_plan(&run_id) {
        fail_missing_lab_attempt_plan(&mut record, &error)?;
        return Err(Error::internal_io(
            format!(
                "cannot bind Lab runner job because durable attempt plan is unavailable: {}",
                error.message
            ),
            Some(run_id),
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
    let accepted_at = record.updated_at.clone();
    let accepted_at = accepted_at.unwrap_or_else(now_timestamp);
    let pending_handoff = record.lab_handoff.clone().unwrap_or_else(|| {
        AgentTaskLabHandoff::pending(
            input.runner_id,
            record.submitted_at.clone(),
            accepted_at.clone(),
        )
    });
    record.lab_handoff = Some(pending_handoff.accepted(input.runner_job_id, accepted_at.clone()));
    let metadata = record.ensure_metadata_object();
    metadata.insert("kind".to_string(), json!("lab_offload_detached_handoff"));
    if let Some(intent) = metadata.get_mut("runner_submission_intent") {
        intent["state"] = json!("accepted");
        intent["runner_job_id"] = json!(input.runner_job_id);
        intent["accepted_at"] = json!(accepted_at);
    }
    metadata.insert(
        "handoff_acceptance".to_string(),
        json!({
            "state": "accepted",
            "accepted_at": accepted_at,
            "runner_job_id": input.runner_job_id,
        }),
    );
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
            homeboy_core::runner_execution_envelope::RunnerExecutionRecord::in_flight(
                input.runner_job_id,
                input.runner_id,
                "daemon",
            )
            .with_job_id(input.runner_job_id)
            .with_agent_task_run_id(&run_id),
        )
        .unwrap_or(Value::Null),
    );
    metadata.insert(METADATA_KEY_RETRYABLE.to_string(), json!(true));
    metadata.remove(METADATA_KEY_STALE_RUNNING);
    metadata.remove(METADATA_KEY_STALE_RUNNING_REASON);
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
    if let Some(problem) = record.lab_handoff_validation_error() {
        return Err(Error::validation_invalid_argument(
            "lab_handoff",
            problem,
            Some(record.run_id.clone()),
            None,
        ));
    }
    if let Some(accepted) = record.lab_handoff.as_ref().filter(|handoff| {
        handoff.state == AgentTaskLabHandoffState::Accepted
            && handoff.authority == AgentTaskLabHandoffAuthority::RunnerDaemon
    }) {
        if accepted.runner_id == runner_id {
            return Ok(record);
        }
        return Err(Error::validation_invalid_argument(
            "runner_id",
            format!(
                "Lab handoff for run '{}' is already accepted by runner '{}'; refusing resume on '{}'",
                record.run_id, accepted.runner_id, runner_id,
            ),
            Some(record.run_id.clone()),
            None,
        ));
    }
    // A previous interruption may have committed the record but not its plan.
    // Repair from the controller-compiled plan before exposing another handoff
    // phase; without it the runner would later create a fake running attempt.
    if store::read_controller_plan(&run_id).is_err() {
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
    record.plan_path = store::controller_plan_path(&run_id)?.display().to_string();
    if record.state.is_terminal() {
        return Ok(record);
    }
    if record.lab_handoff.is_none() {
        let now = chrono::Utc::now();
        record.lab_handoff = Some(AgentTaskLabHandoff::pending(
            runner_id,
            now.to_rfc3339(),
            (now + chrono::Duration::seconds(lab_handoff_acceptance_timeout_seconds()))
                .to_rfc3339(),
        ));
    }
    let metadata = record.ensure_metadata_object();
    metadata.insert("kind".to_string(), json!("lab_offload_controller_proxy"));
    // This record is the controller's durable projection of a runner handoff.
    // It remains controller-owned until a runner-local record is independently
    // discovered, so controller-generated commands must keep resolving here.
    metadata.insert("lifecycle_store_owner".to_string(), json!("controller"));
    metadata.insert("runner_id".to_string(), json!(runner_id));
    if remote_workspace != "pending" {
        metadata.insert("remote_workspace".to_string(), json!(remote_workspace));
    }
    if !remote_command.is_empty() {
        metadata.insert("remote_command".to_string(), json!(remote_command));
    }
    metadata.insert(METADATA_KEY_RETRYABLE.to_string(), json!(true));
    metadata.insert(
        "runner_execution_record".to_string(),
        serde_json::to_value(
            homeboy_core::runner_execution_envelope::RunnerExecutionRecord::planned(
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
    metadata.insert(METADATA_KEY_RETRYABLE.to_string(), json!(true));
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
    let mut plan = load_controller_plan(&source.run_id)?;
    restore_initial_cook_candidate_workspace(&mut plan)?;
    let mut retry = submit_plan(&plan, requested_run_id)?;
    let metadata = retry.ensure_metadata_object();
    if let Some(route) =
        homeboy_core::notification_route::NotificationRoute::from_metadata(&source.metadata)
    {
        // Retries are new durable runs, but retain the initiating route. Resume
        // operates on the same record and therefore needs no copy.
        metadata.insert(
            homeboy_core::notification_route::NOTIFICATION_ROUTE_METADATA_KEY.to_string(),
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

/// Cook's first dirty-candidate baseline is process-local and removed after a
/// failed admission. A retry returns to the durable candidate source workspace;
/// the original task workspace remains in metadata for routing and projection.
fn restore_initial_cook_candidate_workspace(plan: &mut AgentTaskPlan) -> Result<()> {
    for task in &mut plan.tasks {
        let Some(baseline) = task.metadata.get("cook_initial_candidate_baseline") else {
            continue;
        };
        let continuation_root = task
            .metadata
            .pointer("/cook_continuation_workspace/candidate_source_root")
            .and_then(Value::as_str)
            // The first continuation snapshot used `root` for the task
            // workspace. New records use candidate_source_root; retain the
            // legacy form only when no source-root evidence is available.
            .or_else(|| {
                baseline
                    .get("source_root")
                    .and_then(Value::as_str)
                    .or_else(|| {
                        task.metadata
                            .pointer("/cook_continuation_workspace/root")
                            .and_then(Value::as_str)
                    })
            })
            .or_else(|| {
                task.workspace
                    .materialization
                    .get("root")
                    .and_then(Value::as_str)
            })
            // Older records did not retain a continuation workspace separately.
            .or_else(|| baseline.get("source_root").and_then(Value::as_str))
            .filter(|path| !path.trim().is_empty())
            .ok_or_else(|| missing_cook_candidate_source_workspace(&task.task_id, None))?;
        if !std::path::Path::new(continuation_root).is_dir() {
            return Err(missing_cook_candidate_source_workspace(
                &task.task_id,
                Some(continuation_root),
            ));
        }
        task.workspace.root = Some(continuation_root.to_string());
        task.executor.remap_workspace_root(continuation_root);
    }
    Ok(())
}

fn missing_cook_candidate_source_workspace(task_id: &str, root: Option<&str>) -> Error {
    let root_description = root.map(|root| format!(" at {root}")).unwrap_or_default();
    let mut error = Error::validation_invalid_argument(
        "workspace",
        format!(
            "Cook retry candidate source workspace for task '{task_id}' is unavailable{root_description}"
        ),
        root.map(str::to_string),
        None,
    );
    // Losing a managed workspace is lifecycle recovery work, not malformed user
    // input. Callers persist this as a retryable pre-execution failure.
    error.retryable = Some(true);
    error
        .with_hint("Restore the recorded candidate source workspace, then retry the run.")
        .with_hint(
            "If the original --cwd is unavailable, rerun Cook from a replacement workspace with its explicit --cwd.",
        )
}

pub fn logs(run_id: &str) -> Result<AgentTaskRunLog> {
    logs_with_raw(run_id, false)
}

pub fn logs_with_raw(run_id: &str, include_raw: bool) -> Result<AgentTaskRunLog> {
    // Status reconciliation fetches the live daemon snapshot for a bound Lab
    // child, making executor progress visible before the child is terminal.
    let record = status(run_id)?;
    let run_id = record.run_id.clone();
    let (events, artifact_refs, raw_events) = match store::read_aggregate(&run_id) {
        Ok(aggregate) => {
            let refs = artifact_refs_for_outcomes(&aggregate.outcomes);
            (aggregate.events, refs, Vec::new())
        }
        Err(_) => {
            let raw_events = runner_job_raw_events(&record);
            (
                runner_job_progress_events(&record).unwrap_or_else(|| queued_events(&record.tasks)),
                record.artifact_refs.clone(),
                raw_events,
            )
        }
    };
    let events = if raw_events.is_empty() {
        normalize_progress_events(&run_id, &events, &artifact_refs)
    } else {
        normalize_runner_job_events(&run_id, &raw_events, &record, &artifact_refs)
    };
    Ok(AgentTaskRunLog {
        schema: schemas::RUN_LOG.to_string(),
        run_id,
        events,
        raw_events: include_raw.then_some(raw_events).unwrap_or_default(),
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
                message: event
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| {
                        event
                            .pointer("/data/message")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    }),
            })
            .collect(),
    )
}

fn runner_job_raw_events(record: &AgentTaskRunRecord) -> Vec<Value> {
    record
        .metadata
        .get("runner_job_events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn normalize_runner_job_events(
    run_id: &str,
    raw_events: &[Value],
    record: &AgentTaskRunRecord,
    artifact_refs: &[AgentTaskArtifactRef],
) -> Vec<AgentTaskEventEnvelope> {
    let task_id = record
        .tasks
        .first()
        .map(|task| task.task_id.clone())
        .unwrap_or_else(|| record.run_id.clone());
    let provider = record
        .provider_handles
        .first()
        .map(|handle| handle.backend.clone());

    raw_events
        .iter()
        .enumerate()
        .map(|(index, raw)| {
            let data = raw.get("data").cloned().unwrap_or(Value::Null);
            let kind = raw
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("progress");
            let phase =
                string_field(&data, "phase").or_else(|| string_field(&record.metadata, "phase"));
            let activity = string_field(&data, "activity")
                .or_else(|| string_field(&data, "status_note"))
                .or_else(|| string_field(&data, "progress"));
            AgentTaskEventEnvelope {
                schema: schemas::EVENT.to_string(),
                run_id: run_id.to_string(),
                task_id: task_id.clone(),
                // The lifecycle cursor is positional and has always been one-based.
                sequence: (index + 1) as u64,
                event_type: format!("agent_task.runner_{kind}"),
                status: AgentTaskState::Running,
                message: raw
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| string_field(&data, "message")),
                provider: string_field(&data, "provider")
                    .or_else(|| string_field(&data, "backend"))
                    .or_else(|| provider.clone()),
                phase,
                activity,
                heartbeat_at_ms: matches!(kind, "progress" | "status")
                    .then(|| raw.get("timestamp_ms").and_then(Value::as_u64))
                    .flatten(),
                progress: json!({ "attempt": 0 }),
                artifact_refs: artifact_refs
                    .iter()
                    .filter(|reference| reference.task_id == task_id)
                    .cloned()
                    .collect(),
                metadata: data,
            }
        })
        .collect()
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
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

/// Persist the controller publication result separately from promotion so a
/// resumed cook can prove finalization already completed before it publishes.
pub fn record_cook_finalization(run_id: &str, finalization: Value) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let record = store::mutate_record(&run_id, |record| {
        if record.metadata.get("cook_finalization") == Some(&finalization) {
            return false;
        }
        record.updated_at = Some(now_timestamp());
        record
            .ensure_metadata_object()
            .insert("cook_finalization".to_string(), finalization.clone());
        true
    })?;
    match record {
        Some(record) => Ok(record),
        None => store::read_record(&run_id),
    }
}

/// Checkpoint controller-owned recovery after a promoted, green candidate loses
/// its publication base. The terminal provider result remains untouched.
pub fn record_cook_moving_base_recovery(
    run_id: &str,
    recovery: Value,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let record = store::mutate_record(&run_id, |record| {
        if record.metadata.get("cook_moving_base_recovery") == Some(&recovery) {
            return false;
        }
        record.updated_at = Some(now_timestamp());
        record
            .ensure_metadata_object()
            .insert("cook_moving_base_recovery".to_string(), recovery.clone());
        true
    })?;
    match record {
        Some(record) => Ok(record),
        None => store::read_record(&run_id),
    }
}

pub fn clear_cook_moving_base_recovery(run_id: &str) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let record = store::mutate_record(&run_id, |record| {
        let Some(metadata) = record.metadata.as_object_mut() else {
            return false;
        };
        if metadata.remove("cook_moving_base_recovery").is_none() {
            return false;
        }
        record.updated_at = Some(now_timestamp());
        true
    })?;
    match record {
        Some(record) => Ok(record),
        None => store::read_record(&run_id),
    }
}
