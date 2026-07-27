use super::*;
use homeboy_core::api_jobs::RemoteRunnerJobRequest;
use homeboy_engine_primitives::content_hash;
use std::fs::{self, File, OpenOptions};

const LAB_HANDOFF_ACCEPTANCE_TIMEOUT_SECONDS: i64 = 120;
pub(crate) const EXPIRED_LAB_HANDOFF_REASON: &str =
    "runner handoff acceptance deadline expired before a runner job was recorded";

pub(super) fn lab_handoff_acceptance_timeout_seconds() -> i64 {
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
                    let valid_sha = artifact
                        .sha256
                        .as_deref()
                        .is_some_and(content_hash::is_sha256_hex);
                    let content_matches = artifact
                        .path
                        .as_deref()
                        .and_then(|candidate_path| fs::read(candidate_path).ok())
                        .is_some_and(|bytes| {
                            let actual = content_hash::sha256_hex(&bytes);
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

pub(crate) struct LabHandoffLock {
    // Retains the advisory lock until acceptance or expiry has been persisted.
    #[allow(dead_code)]
    file: File,
}

impl LabHandoffLock {
    pub(crate) fn lock(run_id: &str) -> Result<Self> {
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
    submit_plan_with_runtime_admission_on_runner(
        plan,
        requested_run_id,
        execution_runner_id(),
        admit_runtime,
    )
}

pub(crate) fn submit_plan_with_runtime_admission_on_runner<F, A>(
    plan: &AgentTaskPlan,
    requested_run_id: Option<&str>,
    execution_runner_id: Option<String>,
    admit_runtime: F,
) -> Result<AgentTaskRunRecord>
where
    F: FnOnce(&str) -> Result<A>,
    A: RuntimeAdmissionEvidence,
{
    submit_plan_with_runtime_admission_on_runner_with_metadata(
        plan,
        requested_run_id,
        execution_runner_id,
        None,
        admit_runtime,
    )
}

fn submit_plan_with_runtime_admission_on_runner_with_metadata<F, A>(
    plan: &AgentTaskPlan,
    requested_run_id: Option<&str>,
    execution_runner_id: Option<String>,
    submission_metadata: Option<serde_json::Map<String, Value>>,
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
    if let Some(runner_id) = execution_runner_id.as_deref() {
        metadata["runner_id"] = json!(runner_id);
    }
    // Surface controller-owned worktree convergence in the run record as well
    // as the immutable plan, so status and resumed execution retain the same
    // reviewer-facing evidence.
    if let Some(provision) = plan
        .tasks
        .first()
        .and_then(|task| task.metadata.get("worktree_provision"))
    {
        metadata["worktree_provision"] = provision.clone();
    }
    if let Some(route) = homeboy_core::notification_route::current() {
        route.insert_into_metadata(&mut metadata);
    }
    if let Some(submission_metadata) = submission_metadata {
        metadata
            .as_object_mut()
            .expect("submission metadata is an object")
            .extend(submission_metadata);
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
    let mut preserved_controller_runtime = None;
    if let Ok(existing) = store::read_record(&run_id) {
        // A runner may re-submit the plan after the controller reserved a
        // side-effect claim. Claims are durable exactly-once ownership, not
        // plan-derived state, so replacing the record must retain them.
        if let Some(claims) = existing.metadata.get("cook_operation_claims") {
            record.metadata["cook_operation_claims"] = claims.clone();
        }
        // A runner re-submitting a retry must not erase the predecessor identity
        // that makes the reservation discoverable through the indexed lookup.
        for key in ["retry_of", "retry_requested_at", "retry_origin"] {
            if let Some(value) = existing.metadata.get(key) {
                record.metadata[key] = value.clone();
            }
        }
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
                preserved_controller_runtime = Some(controller_runtime_for_runner_execution(
                    &existing,
                    execution_runner_id.as_deref(),
                )?);
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
            if let Some(controller_runtime) = preserved_controller_runtime {
                // The runner executes the portable plan, but the durable run
                // remains owned by the controller that admitted the handoff.
                // Keep their immutable pins distinct across host boundaries.
                record.metadata
                    [homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY] =
                    controller_runtime;
                record.metadata["runner_execution_runtime"] = admission.runtime();
            } else {
                record.metadata
                    [homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY] =
                    admission.runtime();
            }
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

pub(crate) fn controller_runtime_for_runner_execution(
    existing: &AgentTaskRunRecord,
    execution_runner_id: Option<&str>,
) -> Result<Value> {
    if execution_runner_id != existing.runner_id()
        || !existing.lab_handoff.as_ref().is_some_and(|handoff| {
            handoff.state == AgentTaskLabHandoffState::Accepted
                && handoff.authority == AgentTaskLabHandoffAuthority::RunnerDaemon
        })
    {
        return Err(Error::internal_unexpected(
            "runner execution identity was requested for a non-accepted Lab handoff",
        ));
    }
    existing
        .metadata
        .get(homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY)
        .cloned()
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "controller_runtime",
                "accepted Lab handoff has no controller runtime pin",
                Some(existing.run_id.clone()),
                None,
            )
        })
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
    .map_err(|mut error| {
        // A bad pin must never become a recursive re-exec suggestion. Name the
        // narrow repair operation against the durable record instead.
        error.details["next_actions"] = serde_json::json!([format!(
            "homeboy agent-task runtime-recover {} --artifact <trusted-controller-executable>",
            homeboy_core::engine::shell::quote_arg(&record.run_id)
        )]);
        error
    })
}

/// Seal the currently executing controller into an immutable runtime before a
/// new cook begins its local routing and admission work.  Participates in the
/// FIFO admission queue so concurrent seals wait their turn instead of
/// fast-failing.
pub fn pin_current_controller_runtime(
    request_id: &str,
    cancellation_requested: impl Fn() -> Result<bool>,
) -> Result<std::path::PathBuf> {
    let runtime =
        homeboy_core::controller_runtime::pin_current_queued(request_id, cancellation_requested)?;
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
///
/// Retention policy is resolved by core from the operator's configuration; this
/// boundary only forwards the overrides an operator typed.
pub fn prune_controller_runtime_pins(
    apply: bool,
    overrides: homeboy_core::controller_runtime::ControllerRuntimeRetentionOverrides,
) -> Result<homeboy_core::controller_runtime::ControllerRuntimePruneResult> {
    homeboy_core::controller_runtime::prune_pins(apply, overrides)
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
        let started_at = now_timestamp();
        let consumed = {
            let metadata = record.ensure_metadata_object();
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
                "started_at": started_at.clone(),
                // This is the process that owns the synchronous local provider
                // boundary, captured before the scheduler can block on it.
                "owner_pid": std::process::id(),
                "owner_linux_starttime_ticks": homeboy_core::process::linux_process_starttime_ticks(std::process::id()).ok().flatten(),
                "owner_identity": format!("{run_id}:{execution_key}"),
            }));
            let consumed = executions.len();
            metadata.insert("provider_executions_consumed".to_string(), json!(consumed));
            consumed
        };
        let _ = consumed;
        // Advance the heartbeat to provider-execution start for a local
        // (in-process) cook. Before this, a local cook left the heartbeat frozen
        // at submission time for the entire provider run, so operators could not
        // distinguish active execution from a hung preflight (#8396). Restrict
        // this to non-runner-backed runs: a runner-backed run's owner PID and
        // heartbeat are owned by the runner, not the controller reserving here.
        if !record.is_runner_backed() {
            record.updated_at = Some(started_at);
            update_lifecycle_heartbeat(record);
        }
        reservation = ProviderExecutionReservation::Acquired;
        true
    })?;
    Ok(reservation)
}

/// Persist the controller-owned Cook phase independently of provider output.
/// This gives foreground observers a restart-safe liveness source without
/// treating an arbitrary provider transcript line as durable state.
pub fn record_cook_progress(
    run_id: &str,
    phase: &str,
    attempt: u32,
    detail: Option<&str>,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let record = store::mutate_record(&run_id, |record| {
        let now = now_timestamp();
        record.ensure_metadata_object().insert(
            "cook_progress".to_string(),
            json!({
                "phase": phase,
                "attempt": attempt,
                "detail": detail,
                "updated_at": now,
            }),
        );
        if !record.state.is_terminal() && !record.is_runner_backed() {
            record.updated_at = Some(now_timestamp());
            update_lifecycle_heartbeat(record);
        }
        true
    })?;
    record.ok_or_else(|| Error::internal_unexpected("Cook progress record was unchanged"))
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
        let cancelled = record.state == AgentTaskRunState::Cancelled;
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
        // A confirmed cancellation wins over a provider return that raced with
        // it. Conversely, cancellation observes a terminal provider state
        // before it mutates the run, allowing an already-completed result to be
        // imported instead of being discarded.
        if cancelled {
            found = true;
            return false;
        }
        if execution["state"] != json!("running") {
            found = true;
            return false;
        }
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

#[cfg(any(test, feature = "test-support"))]
pub fn rewrite_record_for_test<F>(run_id: &str, mut rewrite: F) -> Result<AgentTaskRunRecord>
where
    F: FnMut(&mut AgentTaskRunRecord),
{
    let mut record = store::read_record(&sanitize_run_id(run_id))?;
    rewrite(&mut record);
    store::write_record(&record)?;
    Ok(record)
}

/// Reconcile the ownership captured at the local provider boundary. A local
/// provider has no opaque remote handle, so its reserving process is the only
/// durable authority that can prove the reservation is still executing.
fn reconcile_local_provider_ownership(record: &mut AgentTaskRunRecord) -> bool {
    if record.state != AgentTaskRunState::Running || record.is_runner_backed() {
        return false;
    }
    let Some(executions) = record
        .metadata
        .get_mut("provider_executions")
        .and_then(Value::as_array_mut)
    else {
        return false;
    };

    let mut has_reconcilable_execution = false;
    let mut has_live_owner = false;
    let mut has_unverifiable_owner = false;
    let mut has_succeeded = false;
    let mut recovery_identity = Vec::new();
    for execution in executions.iter_mut() {
        match execution["state"].as_str() {
            Some("running") | Some("succeeded") => {
                has_reconcilable_execution = true;
                has_succeeded |= execution["state"] == json!("succeeded");
                recovery_identity.push(execution["owner_identity"].clone());
                let identity_state = execution
                    .get("owner_pid")
                    .and_then(Value::as_u64)
                    .and_then(|pid| u32::try_from(pid).ok())
                    .map(|pid| {
                        homeboy_core::process::process_identity_state(
                            pid,
                            execution
                                .get("owner_linux_starttime_ticks")
                                .and_then(Value::as_u64),
                        )
                    });
                let live = matches!(
                    identity_state,
                    Some(homeboy_core::process::ProcessIdentityState::Live)
                );
                has_unverifiable_owner |= !matches!(
                    identity_state,
                    Some(
                        homeboy_core::process::ProcessIdentityState::Live
                            | homeboy_core::process::ProcessIdentityState::Dead
                            | homeboy_core::process::ProcessIdentityState::IdentityMismatch
                    )
                );
                execution["owner_state"] = json!(match identity_state {
                    Some(homeboy_core::process::ProcessIdentityState::Live) => "live",
                    Some(homeboy_core::process::ProcessIdentityState::Dead) => "dead",
                    Some(homeboy_core::process::ProcessIdentityState::IdentityMismatch) =>
                        "identity_mismatch",
                    _ => "unverifiable",
                });
                has_live_owner |= live;
            }
            _ => {}
        }
    }
    // Older records predate per-provider ownership, and non-Linux hosts can be
    // unable to verify a persisted identity. Neither is proof that the owner
    // died, so retain the joinable run instead of terminalizing it on a read.
    if has_live_owner || has_unverifiable_owner {
        return true;
    }
    if !has_reconcilable_execution {
        return false;
    }

    let now = now_timestamp();
    let metadata = record.ensure_metadata_object();
    metadata.insert(
        "local_provider_ownership".to_string(),
        json!({
            "state": "owner_dead",
            "recovery_identity": recovery_identity,
            "reconciled_at": now,
        }),
    );
    if has_succeeded {
        // The provider reported completion before the foreground owner died,
        // but the aggregate was not yet persisted. Preserve that fact and the
        // workspace as a recoverable candidate instead of erasing it.
        record.updated_at = Some(now);
        set_run_state(record, AgentTaskRunState::CandidateRecoverable);
        for task in &mut record.tasks {
            if task.state == AgentTaskState::Running {
                task.state = AgentTaskState::CandidateRecoverable;
            }
        }
    } else {
        let executions = record
            .ensure_metadata_object()
            .get_mut("provider_executions")
            .and_then(Value::as_array_mut)
            .expect("provider executions were checked above");
        for execution in executions.iter_mut() {
            if execution["state"] == json!("running") {
                execution["state"] = json!("cancelled");
                execution["finished_at"] = json!(now.clone());
            }
        }
        record.updated_at = Some(now.clone());
        set_run_state(record, AgentTaskRunState::Cancelled);
        for task in &mut record.tasks {
            if matches!(task.state, AgentTaskState::Queued | AgentTaskState::Running) {
                task.state = AgentTaskState::Cancelled;
            }
        }
        record.ensure_metadata_object().insert(
            "cancel_reason".to_string(),
            json!("local provider owner process is not running"),
        );
    }
    true
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

/// Whether answering for this record can require reaching the runner at all.
///
/// A controller-local run — no runner id, no runner job id, and no Lab handoff —
/// is fully described by durable controller state. Answering for it must never
/// depend on a runner being reachable (#10418): the sharpest symptom of the
/// wedged-Lab outage was `agent-task status <id>` failing to return a *known
/// controller-local* run because the read path unconditionally entered runner
/// reconciliation.
pub fn is_controller_local(record: &AgentTaskRunRecord) -> bool {
    record.runner_id().is_none() && record.runner_job_id().is_none() && record.lab_handoff.is_none()
}

/// Whether a read-side `status()` may reach the runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentTaskRunnerProbe {
    /// Reach the runner only when the record is genuinely runner-backed and
    /// still running. This is the historical behavior for Lab runs and the
    /// default.
    #[default]
    WhenRunnerBacked,
    /// Never reach the runner. The answer comes from durable controller state
    /// alone and is labelled as such.
    Never,
}

/// Read-side options for [`status_with_options`].
#[derive(Debug, Clone, Copy, Default)]
pub struct AgentTaskStatusOptions {
    pub runner_probe: AgentTaskRunnerProbe,
}

/// What the read path actually did about the runner, reported alongside the
/// record so a caller can tell a complete answer from a deliberately-local one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AgentTaskRunnerProbePlan {
    /// Whether runner reconciliation was performed for this read.
    pub performed: bool,
    /// Why the runner was not reached, when it was not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped_reason: Option<&'static str>,
    /// Whether this record is answerable entirely from controller-local state.
    pub controller_local: bool,
}

/// The record is fully controller-local; the runner is irrelevant to the answer.
pub const RUNNER_PROBE_SKIPPED_CONTROLLER_LOCAL: &str = "controller_local_record";
/// The caller explicitly asked for a local-only answer.
pub const RUNNER_PROBE_SKIPPED_CALLER_OPTED_OUT: &str = "caller_opted_out";
/// A terminal / non-running record has no live runner job to reconcile.
pub const RUNNER_PROBE_SKIPPED_NOT_RUNNING: &str = "run_is_not_running";

/// Decide whether this read touches the runner. Pure so the "controller-local
/// answers locally" contract is directly testable.
pub fn runner_probe_plan(
    record: &AgentTaskRunRecord,
    options: AgentTaskStatusOptions,
) -> AgentTaskRunnerProbePlan {
    let controller_local = is_controller_local(record);
    let skipped_reason = if controller_local {
        Some(RUNNER_PROBE_SKIPPED_CONTROLLER_LOCAL)
    } else if options.runner_probe == AgentTaskRunnerProbe::Never {
        Some(RUNNER_PROBE_SKIPPED_CALLER_OPTED_OUT)
    } else if record.state != AgentTaskRunState::Running {
        Some(RUNNER_PROBE_SKIPPED_NOT_RUNNING)
    } else {
        None
    };
    AgentTaskRunnerProbePlan {
        performed: skipped_reason.is_none(),
        skipped_reason,
        controller_local,
    }
}

/// A durable run record plus the read-side runner-probe decision that produced
/// it.
#[derive(Debug, Clone)]
pub struct AgentTaskStatusOutcome {
    pub record: AgentTaskRunRecord,
    pub runner_probe: AgentTaskRunnerProbePlan,
}

/// Read the durable run record with live reconciliation applied (deferred
/// candidate, runtime admission, and runner/daemon status projection) so callers
/// see the current, joinable controller record.
pub fn status(run_id: &str) -> Result<AgentTaskRunRecord> {
    Ok(status_with_options(run_id, AgentTaskStatusOptions::default())?.record)
}

/// [`status`] with explicit control over whether the read may reach the runner.
///
/// A controller-local record is always answered locally, regardless of options.
pub fn status_with_options(
    run_id: &str,
    options: AgentTaskStatusOptions,
) -> Result<AgentTaskStatusOutcome> {
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
    // A daemon can evict a completed job from its active store before a restarted
    // controller observes it. The terminal event log already mirrored into this
    // observation record is sufficient to recover the aggregate and artifacts.
    // Consume it before querying the live runner, which is no longer authority
    // once its active entry has been evicted.
    if project_persisted_terminal_runner_events(&mut record)? {
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
    if reconcile_local_provider_ownership(&mut record) {
        store::write_record(&record)?;
    }
    // The only genuinely-remote step in this read. Skipping it for a
    // controller-local record is what makes `agent-task status` answerable while
    // the Lab is wedged (#10418).
    let runner_probe = runner_probe_plan(&record, options);
    let before_liveness_reconciliation = record.clone();
    if runner_probe.performed {
        reconcile_runner_job_state(&mut record)?;
    }
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
            // Reproject authoritative aggregate model evidence onto a terminal
            // record whose durable lifecycle model is stale-null (#9411). A run
            // that went terminal before the #9404/#9405 model repair keeps
            // `provider_runtime[].metadata.model = null`, which blocks
            // `finalize-pr` even though the aggregate recorded a concrete model.
            if crate::agent_task_lifecycle::terminal_provider_model_reconciliation_needed(
                &record, &aggregate,
            ) {
                let controller_plan = store::read_controller_plan(&record.run_id)?;
                let projection_plan = aggregate_projection_plan(&controller_plan, &aggregate);
                crate::agent_task_lifecycle::reconcile_terminal_provider_model(
                    &mut record,
                    &projection_plan,
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
    Ok(AgentTaskStatusOutcome {
        record,
        runner_probe,
    })
}

/// Read the controller-owned lifecycle record without contacting its runner.
///
/// This is deliberately separate from [`status`]: runner reconciliation is
/// authoritative when an operator explicitly asks to refresh liveness, but a
/// wedged runner must never hide state already persisted by the controller.
pub fn persisted_status(run_id: &str) -> Result<AgentTaskRunRecord> {
    let resolved_run_id = resolve_run_id(run_id)?;
    store::read_record(&resolved_run_id)
}

/// Refresh accepted runner handoffs and expire unbound controller handoffs before
/// a read model (such as activity) projects lifecycle state. A controller wait
/// expiry is not terminal after a runner job is recorded: the runner daemon
/// remains the authority until it reports a terminal job result.

pub fn run_status(run_id: &str, since_cursor: Option<u64>) -> Result<AgentTaskRunStatus> {
    let record = status(run_id)?;
    let aggregate = store::read_aggregate(&record.run_id).ok();
    let (events, artifact_refs) = match aggregate.as_ref() {
        Some(aggregate) => {
            let refs = artifact_refs_for_outcomes(&aggregate.outcomes);
            (aggregate.events.clone(), refs)
        }
        None => {
            // Surface a local cook's durable running provider execution so the
            // live bridge status advances past "task submitted" too (#8396).
            let mut events = queued_events(&record.tasks);
            events.extend(local_provider_execution_events(&record));
            (events, record.artifact_refs.clone())
        }
    };
    let candidate = load_plan_for_execution(&record.run_id)
        .ok()
        .and_then(|plan| {
            (plan.tasks.len() > 1).then(|| {
                let selected = aggregate
                    .as_ref()
                    .and_then(|value| value.selected_outcome());
                AgentTaskCandidateStatus {
                    policy: plan.options.candidate_completion,
                    selected_task_id: selected.map(|outcome| outcome.task_id.clone()),
                    candidates: aggregate
                        .as_ref()
                        .map(|value| tasks_for_aggregate(&plan, value))
                        .unwrap_or_else(|| record.tasks.clone()),
                    deadline_timeout_ms: plan.options.timeout_ms,
                    cancellation_supervision: if selected.is_some() {
                        "scheduler_deferred_cleanup".to_string()
                    } else {
                        "controller_owned".to_string()
                    },
                    promotion_action: selected.and_then(|outcome| {
                        outcome.metadata["candidate_selection"]["promotion_action"]
                            .as_str()
                            .map(str::to_string)
                    }),
                }
            })
        });
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
        candidate,
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
    super::cook_workspace_restore::restore_initial_cook_candidate_workspace(&mut plan)?;
    super::cook_workspace_restore::restore_follow_up_cook_candidate_workspace(&mut plan)?;
    let mut metadata = serde_json::Map::new();
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
    submit_plan_with_runtime_admission_on_runner_with_metadata(
        &plan,
        requested_run_id,
        execution_runner_id(),
        Some(metadata),
        |run_id| {
            homeboy_core::controller_runtime::admit_current_for_with_cancellation_check(
                run_id,
                || Ok(store::read_record(run_id)?.state.is_terminal()),
            )
        },
    )
}

/// Find the one lifecycle-first Cook retry reservation that can be bound to an
/// unbound recipe attempt. The `retry_of` lookup is backed by the observation
/// metadata index; the plan and attempt-shaped run id prevent adoption of an
/// unrelated retry from the same source.
pub fn find_unbound_cook_retry_successor(
    source_run_id: &str,
    cook_id: &str,
    attempt: u32,
    plan: &AgentTaskPlan,
) -> Result<Option<AgentTaskRunRecord>> {
    let prefix = format!("{}-attempt-{attempt}-", sanitize_run_id(cook_id));
    let mut matches = store::read_retry_successors(&sanitize_run_id(source_run_id))?
        .into_iter()
        .filter(|record| record.run_id.starts_with(&prefix))
        .filter(|record| record.plan_id == plan.plan_id)
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| left.run_id.cmp(&right.run_id));
    match matches.as_slice() {
        [] => Ok(None),
        [record] => Ok(Some(record.clone())),
        _ => Err(Error::validation_invalid_argument(
            "cook_recipe.attempts",
            "multiple lifecycle retry reservations match this Cook attempt",
            Some(source_run_id.to_string()),
            None,
        )),
    }
}

/// Rebuild a gate-failed Cook candidate from its controller-owned promotion
/// before retrying. A persisted follow-up plan names a temporary checkout, not
/// authority to reuse whatever happens to exist at that path.

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

/// Record the controller-owned boundary that a resumed Cook must advance.
/// Provider terminal evidence and promotion reports remain separate so a later
/// failed attempt cannot replace the source candidate's recovery checkpoint.
pub fn record_cook_recovery_checkpoint(
    run_id: &str,
    phase: &str,
    next_command: &str,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let checkpoint = json!({
        "schema": "homeboy/agent-task-cook-recovery-checkpoint/v1",
        "phase": phase,
        "next_command": next_command,
    });
    let record = store::mutate_record(&run_id, |record| {
        if record.metadata.get("cook_recovery_checkpoint") == Some(&checkpoint) {
            return false;
        }
        record.updated_at = Some(now_timestamp());
        record
            .ensure_metadata_object()
            .insert("cook_recovery_checkpoint".to_string(), checkpoint.clone());
        true
    })?;
    match record {
        Some(record) => Ok(record),
        None => store::read_record(&run_id),
    }
}

pub fn cook_index(cook_id: &str) -> Result<AgentTaskCookIndex> {
    store::read_cook_index(&sanitize_run_id(cook_id))
}

/// Read one durable attempt without resolving a cook ID through its latest
/// index entry. Recovery must inspect historical source attempts directly.
pub fn exact_record(run_id: &str) -> Result<AgentTaskRunRecord> {
    store::read_record(&sanitize_run_id(run_id))
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
        // The first post-apply checkpoint is immutable recovery authority. Later
        // gate/finalization reports advance `latest_promotion` without obscuring
        // the exact candidate that may be resumed after a coordinator loss.
        if promotion.get("status").and_then(Value::as_str) == Some("verification_pending") {
            metadata
                .entry("cook_recovery_source_checkpoint".to_string())
                .or_insert_with(|| promotion.clone());
        }
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
