//! Owning service for durable agent-task submission of an already prepared plan.
//!
//! Adapters prepare and admit plans upstream. This service owns one sequence:
//! durable submit or existing-record reuse, queue-only acknowledgement or a
//! legal running transition, scheduler invocation, aggregate persistence, and
//! the canonical control-plane submission projection.

use homeboy_control_plane_contract::{
    ControlPlaneActionOutcome, ControlPlaneSubmissionAcknowledgement,
    ControlPlaneSubmissionRequest, RunId, CONTROL_PLANE_SUBMISSION_ACKNOWLEDGEMENT_SCHEMA,
    CONTROL_PLANE_SUBMISSION_REQUEST_SCHEMA,
};

use crate::agent_task_lifecycle::{
    self, AgentTaskLifecycleStore, AgentTaskRunRecord, AgentTaskRunState,
};
use crate::agent_task_scheduler::{
    AgentTaskAggregate, AgentTaskPlan, AgentTaskScheduler, HarvestExecutionContext,
    SharedAgentTaskExecutor,
};
use crate::agent_task_service::{
    aggregate_exit_code, bind_runner_snapshot_workspace_attestations, DerivedCookBaselineCapability,
};
use homeboy_core::{Error, Result};
use serde_json::{json, Map, Value};
use std::fs::OpenOptions;

const SUBMISSION_METADATA_KEY: &str = "control_plane_submission";
const SUBMISSION_METADATA_SCHEMA: &str = "homeboy/control-plane-submission/v1";

pub struct PreparedAgentTaskSubmission {
    plan: AgentTaskPlan,
    lifecycle_store: Option<AgentTaskLifecycleStore>,
    harvest_context: Option<HarvestExecutionContext>,
    queued_plan_enrichment: bool,
    claimed_plan_enrichment: bool,
}

impl PreparedAgentTaskSubmission {
    pub fn new(plan: AgentTaskPlan) -> Self {
        Self {
            plan,
            lifecycle_store: None,
            harvest_context: None,
            queued_plan_enrichment: false,
            claimed_plan_enrichment: false,
        }
    }

    pub fn with_lifecycle_store(mut self, store: AgentTaskLifecycleStore) -> Self {
        self.lifecycle_store = Some(store);
        self
    }

    pub fn with_harvest_context(mut self, harvest_context: HarvestExecutionContext) -> Self {
        self.harvest_context = Some(harvest_context);
        self
    }

    pub(crate) fn with_queued_plan_enrichment(mut self) -> Self {
        self.queued_plan_enrichment = true;
        self
    }

    pub(crate) fn with_claimed_plan_enrichment(mut self) -> Self {
        self.claimed_plan_enrichment = true;
        self
    }
}

#[derive(Debug)]
pub struct AgentTaskSubmissionOutcome {
    pub acknowledgement: ControlPlaneSubmissionAcknowledgement,
    pub submitted: AgentTaskRunRecord,
    pub record: AgentTaskRunRecord,
    pub aggregate: Option<AgentTaskAggregate>,
    pub exit_code: i32,
}

#[derive(Clone)]
struct SubmissionIdentity {
    idempotency_key: String,
    actor: String,
    accepted_at: String,
}

pub fn prepared_submission_request(
    run_id: Option<&str>,
    queue_only: bool,
    actor: impl Into<String>,
) -> Result<ControlPlaneSubmissionRequest> {
    let run = match run_id {
        Some(run_id) => RunId::new(run_id).map_err(|error| {
            Error::validation_invalid_argument(
                "run_id",
                error.to_string(),
                Some(run_id.to_string()),
                None,
            )
        })?,
        None => RunId::new(format!("agent-task-{}", uuid::Uuid::new_v4()))
            .expect("generated run id is valid"),
    };
    Ok(ControlPlaneSubmissionRequest {
        schema: CONTROL_PLANE_SUBMISSION_REQUEST_SCHEMA.to_string(),
        idempotency_key: run.as_str().to_string(),
        actor: actor.into(),
        run,
        queue_only,
    })
}

pub fn submit_prepared_plan(
    request: &ControlPlaneSubmissionRequest,
    prepared: PreparedAgentTaskSubmission,
    executor: SharedAgentTaskExecutor,
) -> Result<AgentTaskSubmissionOutcome> {
    submit_prepared_plan_inner(request, prepared, Some(executor), None, |_| Ok(()))
}

pub fn queue_prepared_plan(
    request: &ControlPlaneSubmissionRequest,
    prepared: PreparedAgentTaskSubmission,
) -> Result<AgentTaskSubmissionOutcome> {
    if !request.queue_only {
        return Err(Error::validation_invalid_argument(
            "queue_only",
            "queue submission requires queue_only=true",
            None,
            None,
        ));
    }
    submit_prepared_plan_inner(request, prepared, None, None, |_| Ok(()))
}

pub(crate) fn submit_prepared_plan_with_cook_baseline(
    request: &ControlPlaneSubmissionRequest,
    prepared: PreparedAgentTaskSubmission,
    executor: SharedAgentTaskExecutor,
    derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
) -> Result<AgentTaskSubmissionOutcome> {
    submit_prepared_plan_inner(
        request,
        prepared,
        Some(executor),
        derived_cook_baseline,
        |_| Ok(()),
    )
}

pub(crate) fn submit_prepared_plan_with_observer<F>(
    request: &ControlPlaneSubmissionRequest,
    prepared: PreparedAgentTaskSubmission,
    executor: SharedAgentTaskExecutor,
    on_submitted: F,
) -> Result<AgentTaskSubmissionOutcome>
where
    F: FnOnce(&AgentTaskRunRecord) -> Result<()>,
{
    submit_prepared_plan_inner(request, prepared, Some(executor), None, on_submitted)
}

pub(crate) fn queue_prepared_plan_with_observer<F>(
    request: &ControlPlaneSubmissionRequest,
    prepared: PreparedAgentTaskSubmission,
    on_submitted: F,
) -> Result<AgentTaskSubmissionOutcome>
where
    F: FnOnce(&AgentTaskRunRecord) -> Result<()>,
{
    if !request.queue_only {
        return Err(Error::validation_invalid_argument(
            "queue_only",
            "queue submission requires queue_only=true",
            None,
            None,
        ));
    }
    submit_prepared_plan_inner(request, prepared, None, None, on_submitted)
}

pub(crate) fn execute_claimed_plan(
    run_id: &str,
    mut prepared: PreparedAgentTaskSubmission,
    executor: SharedAgentTaskExecutor,
) -> Result<AgentTaskSubmissionOutcome> {
    let request = prepared_submission_request(Some(run_id), false, "homeboy-claimed-run")?;
    let lifecycle_store = prepared
        .lifecycle_store
        .clone()
        .map(Ok)
        .unwrap_or_else(AgentTaskLifecycleStore::from_current_environment)?;
    prepared.lifecycle_store = Some(lifecycle_store.clone());
    with_submission_lock(&lifecycle_store, run_id, || {
        let existing = lifecycle_store.read_record(run_id)?;
        let identity = submission_identity(&request, &prepared.plan, Some(&existing), false, true)?;
        if let Some(outcome) =
            terminal_reuse_outcome(&identity, &lifecycle_store, existing.clone())?
        {
            return Ok(outcome);
        }
        if existing.state != AgentTaskRunState::Running {
            return Err(Error::validation_invalid_argument(
                "run",
                format!(
                    "claimed agent-task run '{}' must be running before execution",
                    existing.run_id
                ),
                Some(existing.run_id),
                None,
            ));
        }

        let harvest_context = match prepared
            .harvest_context
            .take()
            .map(Ok)
            .unwrap_or_else(HarvestExecutionContext::from_current_process)
        {
            Ok(context) => context,
            Err(error) => {
                lifecycle_store.record_pre_execution_failure(
                    run_id,
                    &prepared.plan,
                    "validate_harvest_transport",
                    &error,
                )?;
                return Err(error);
            }
        };
        if harvest_context.snapshot_signaled() {
            bind_runner_snapshot_workspace_attestations(&mut prepared.plan)?;
        }
        lifecycle_store.write_controller_plan(run_id, &prepared.plan)?;
        let binding = submission_metadata(&request, &prepared.plan, &identity)
            .remove(SUBMISSION_METADATA_KEY)
            .expect("submission binding");
        lifecycle_store.mutate_record(run_id, |record| {
            record.metadata[SUBMISSION_METADATA_KEY] = binding;
            record.updated_at = Some(agent_task_lifecycle::now_timestamp());
            true
        })?;

        let aggregate = run_with_scheduler(
            &lifecycle_store,
            prepared.plan.clone(),
            run_id,
            executor,
            None,
            harvest_context,
        )?;
        let record = lifecycle_store.record_run_aggregate(run_id, &prepared.plan, &aggregate)?;
        acknowledgement_outcome(
            &identity,
            existing,
            Some(record),
            Some(aggregate.clone()),
            aggregate_exit_code(&aggregate),
            false,
            ControlPlaneActionOutcome::Succeeded,
        )
    })
}

pub(crate) fn stage_prepared_plan(
    request: &ControlPlaneSubmissionRequest,
    prepared: PreparedAgentTaskSubmission,
) -> Result<AgentTaskRunRecord> {
    validate_submission_request(request)?;
    let lifecycle_store = prepared
        .lifecycle_store
        .clone()
        .map(Ok)
        .unwrap_or_else(AgentTaskLifecycleStore::from_current_environment)?;
    with_submission_lock(&lifecycle_store, request.run.as_str(), || {
        let existing = load_existing_record(&lifecycle_store, request.run.as_str())?;
        let identity = submission_identity(
            request,
            &prepared.plan,
            existing.as_ref(),
            prepared.queued_plan_enrichment,
            prepared.claimed_plan_enrichment,
        )?;
        if let Some(existing) = existing {
            if existing.state.is_terminal()
                && !crate::agent_task_service::cook_pre_execution::retryable_pre_execution_failure(
                    &existing,
                )
            {
                return Ok(existing);
            }
            if existing.state == AgentTaskRunState::Running {
                return Err(Error::validation_invalid_argument(
                    "run",
                    format!("agent-task run '{}' is already running", existing.run_id),
                    Some(existing.run_id),
                    None,
                )
                .with_retryable(true));
            }
        }
        persist_plan(
            &lifecycle_store,
            &prepared.plan,
            request.run.as_str(),
            submission_metadata(request, &prepared.plan, &identity),
        )
    })
}

pub(crate) fn execute_ephemeral_prepared_plan(
    mut prepared: PreparedAgentTaskSubmission,
    executor: SharedAgentTaskExecutor,
    derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
) -> Result<AgentTaskAggregate> {
    let harvest_context = prepared
        .harvest_context
        .take()
        .map(Ok)
        .unwrap_or_else(HarvestExecutionContext::from_current_process)?;
    if harvest_context.snapshot_signaled() {
        bind_runner_snapshot_workspace_attestations(&mut prepared.plan)?;
    }
    let scheduler =
        AgentTaskScheduler::new_controller(executor).with_harvest_context(harvest_context);
    let scheduler = match prepared.lifecycle_store {
        Some(store) => scheduler.with_lifecycle_store(store),
        None => scheduler,
    };
    Ok(scheduler.run_with_derived_cook_baseline(prepared.plan, derived_cook_baseline))
}

pub(crate) fn reject_prepared_plan(
    request: &ControlPlaneSubmissionRequest,
    prepared: PreparedAgentTaskSubmission,
    phase: &str,
    error: &Error,
) -> Result<AgentTaskRunRecord> {
    validate_submission_request(request)?;
    let lifecycle_store = prepared
        .lifecycle_store
        .clone()
        .map(Ok)
        .unwrap_or_else(AgentTaskLifecycleStore::from_current_environment)?;
    with_submission_lock(&lifecycle_store, request.run.as_str(), || {
        let existing = load_existing_record(&lifecycle_store, request.run.as_str())?;
        let identity = submission_identity(
            request,
            &prepared.plan,
            existing.as_ref(),
            prepared.queued_plan_enrichment,
            prepared.claimed_plan_enrichment,
        )?;
        if existing.as_ref().is_some_and(|record| {
            record.state.is_terminal()
                && !crate::agent_task_service::cook_pre_execution::retryable_pre_execution_failure(
                    record,
                )
        }) {
            return Ok(existing.expect("terminal record"));
        }
        let submitted = if existing
            .as_ref()
            .is_some_and(|record| record.state == AgentTaskRunState::Running)
        {
            lifecycle_store.write_controller_plan(request.run.as_str(), &prepared.plan)?;
            let binding = submission_metadata(request, &prepared.plan, &identity)
                .remove(SUBMISSION_METADATA_KEY)
                .expect("submission binding");
            lifecycle_store.mutate_record(request.run.as_str(), |record| {
                record.metadata[SUBMISSION_METADATA_KEY] = binding;
                record.updated_at = Some(agent_task_lifecycle::now_timestamp());
                true
            })?;
            lifecycle_store.read_record(request.run.as_str())?
        } else {
            persist_plan(
                &lifecycle_store,
                &prepared.plan,
                request.run.as_str(),
                submission_metadata(request, &prepared.plan, &identity),
            )?
        };
        lifecycle_store.record_pre_execution_failure(
            &submitted.run_id,
            &prepared.plan,
            phase,
            error,
        )
    })
}

fn submit_prepared_plan_inner<F>(
    request: &ControlPlaneSubmissionRequest,
    mut prepared: PreparedAgentTaskSubmission,
    executor: Option<SharedAgentTaskExecutor>,
    derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    on_submitted: F,
) -> Result<AgentTaskSubmissionOutcome>
where
    F: FnOnce(&AgentTaskRunRecord) -> Result<()>,
{
    validate_submission_request(request)?;
    let lifecycle_store = prepared
        .lifecycle_store
        .clone()
        .map(Ok)
        .unwrap_or_else(AgentTaskLifecycleStore::from_current_environment)?;
    prepared.lifecycle_store = Some(lifecycle_store.clone());
    with_submission_lock(&lifecycle_store, request.run.as_str(), || {
        submit_prepared_plan_locked(
            request,
            prepared,
            executor,
            derived_cook_baseline,
            on_submitted,
        )
    })
}

fn submit_prepared_plan_locked<F>(
    request: &ControlPlaneSubmissionRequest,
    mut prepared: PreparedAgentTaskSubmission,
    executor: Option<SharedAgentTaskExecutor>,
    derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    on_submitted: F,
) -> Result<AgentTaskSubmissionOutcome>
where
    F: FnOnce(&AgentTaskRunRecord) -> Result<()>,
{
    let requested_run_id = request.run.as_str();
    let lifecycle_store = prepared
        .lifecycle_store
        .as_ref()
        .expect("submission store resolved before lock");

    let existing = load_existing_record(lifecycle_store, requested_run_id)?;
    let identity = submission_identity(
        request,
        &prepared.plan,
        existing.as_ref(),
        prepared.queued_plan_enrichment,
        prepared.claimed_plan_enrichment,
    )?;
    if let Some(existing) = existing {
        if let Some(outcome) = terminal_reuse_outcome(&identity, lifecycle_store, existing.clone())?
        {
            on_submitted(&existing)?;
            return Ok(outcome);
        }
        if existing.state == AgentTaskRunState::Running
            && !accepted_runner_handoff_can_materialize(&existing)
        {
            return Err(Error::validation_invalid_argument(
                "run",
                format!("agent-task run '{}' is already running", existing.run_id),
                Some(existing.run_id),
                None,
            )
            .with_retryable(true));
        }
    }

    let submitted = persist_plan(
        lifecycle_store,
        &prepared.plan,
        requested_run_id,
        submission_metadata(request, &prepared.plan, &identity),
    )?;
    on_submitted(&submitted)?;
    if request.queue_only {
        return acknowledgement_outcome(
            &identity,
            submitted,
            None,
            None,
            0,
            true,
            ControlPlaneActionOutcome::Succeeded,
        );
    }
    let executor = executor.ok_or_else(|| {
        Error::validation_invalid_argument(
            "queue_only",
            "executing submission requires an executor",
            None,
            None,
        )
    })?;

    let run_id = submitted.run_id.clone();
    let harvest_context = match prepared
        .harvest_context
        .take()
        .map(Ok)
        .unwrap_or_else(HarvestExecutionContext::from_current_process)
    {
        Ok(context) => context,
        Err(error) => {
            record_pre_execution_failure(
                lifecycle_store,
                &run_id,
                &prepared.plan,
                "validate_harvest_transport",
                &error,
            )?;
            return Err(error);
        }
    };
    if harvest_context.snapshot_signaled() {
        bind_runner_snapshot_workspace_attestations(&mut prepared.plan)?;
    }
    mark_running(lifecycle_store, &run_id)?;
    let aggregate = run_with_scheduler(
        lifecycle_store,
        prepared.plan.clone(),
        &run_id,
        executor,
        derived_cook_baseline,
        harvest_context,
    )?;
    let record = persist_aggregate(lifecycle_store, &run_id, &prepared.plan, &aggregate)?;
    acknowledgement_outcome(
        &identity,
        submitted,
        Some(record),
        Some(aggregate.clone()),
        aggregate_exit_code(&aggregate),
        false,
        ControlPlaneActionOutcome::Succeeded,
    )
}

fn validate_submission_request(request: &ControlPlaneSubmissionRequest) -> Result<()> {
    if request.schema != CONTROL_PLANE_SUBMISSION_REQUEST_SCHEMA {
        return Err(Error::validation_invalid_argument(
            "schema",
            format!(
                "control-plane submission request schema must be {CONTROL_PLANE_SUBMISSION_REQUEST_SCHEMA}"
            ),
            None,
            None,
        ));
    }
    if request.idempotency_key.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "idempotency_key",
            "control-plane submission request requires an idempotency key",
            None,
            None,
        ));
    }
    if request.actor.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "actor",
            "control-plane submission request requires an actor",
            None,
            None,
        ));
    }
    if request.idempotency_key != request.run.as_str() {
        return Err(Error::validation_invalid_argument(
            "idempotency_key",
            "control-plane submission idempotency key must equal the canonical run id",
            Some(request.idempotency_key.clone()),
            None,
        ));
    }
    Ok(())
}

fn load_existing_record(
    store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<Option<AgentTaskRunRecord>> {
    if !store.record_exists(run_id)? {
        return Ok(None);
    }
    store.read_record(run_id).map(Some)
}

fn terminal_reuse_outcome(
    identity: &SubmissionIdentity,
    store: &AgentTaskLifecycleStore,
    existing: AgentTaskRunRecord,
) -> Result<Option<AgentTaskSubmissionOutcome>> {
    if crate::agent_task_service::cook_pre_execution::retryable_pre_execution_failure(&existing) {
        return Ok(None);
    }
    if !existing.state.is_terminal() {
        return Ok(None);
    }
    let aggregate = match store.read_aggregate(&existing.run_id) {
        Ok(aggregate) => crate::agent_task_artifacts::reviewer_facing_aggregate(&aggregate),
        Err(_) => {
            return Err(Error::validation_invalid_argument(
                "run_id",
                format!(
                    "agent-task run '{}' is terminal with state {:?} but has no durable aggregate evidence",
                    existing.run_id, existing.state
                ),
                Some(existing.run_id.clone()),
                Some(vec![format!(
                    "retry the child with: homeboy agent-task retry {} --run",
                    existing.run_id
                )]),
            ));
        }
    };
    acknowledgement_outcome(
        identity,
        existing,
        None,
        Some(aggregate.clone()),
        aggregate_exit_code(&aggregate),
        false,
        ControlPlaneActionOutcome::AlreadySatisfied,
    )
    .map(Some)
}

fn persist_plan(
    store: &AgentTaskLifecycleStore,
    plan: &AgentTaskPlan,
    run_id: &str,
    submission_metadata: Map<String, Value>,
) -> Result<AgentTaskRunRecord> {
    store.submit_plan_with_current_runtime_and_metadata(plan, run_id, Some(submission_metadata))
}

fn record_pre_execution_failure(
    store: &AgentTaskLifecycleStore,
    run_id: &str,
    plan: &AgentTaskPlan,
    phase: &str,
    error: &Error,
) -> Result<AgentTaskRunRecord> {
    store.record_pre_execution_failure(run_id, plan, phase, error)
}

fn mark_running(store: &AgentTaskLifecycleStore, run_id: &str) -> Result<AgentTaskRunRecord> {
    store.mark_running(run_id)
}

fn persist_aggregate(
    store: &AgentTaskLifecycleStore,
    run_id: &str,
    plan: &AgentTaskPlan,
    aggregate: &AgentTaskAggregate,
) -> Result<AgentTaskRunRecord> {
    store.record_run_aggregate(run_id, plan, aggregate)
}

fn run_with_scheduler(
    store: &AgentTaskLifecycleStore,
    plan: AgentTaskPlan,
    run_id: &str,
    executor: SharedAgentTaskExecutor,
    derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    harvest_context: HarvestExecutionContext,
) -> Result<AgentTaskAggregate> {
    let scheduler = AgentTaskScheduler::new_controller(executor)
        .with_harvest_context(harvest_context)
        .with_lifecycle_store(store.clone());
    Ok(scheduler
        .with_run_id(run_id.to_string())
        .run_with_derived_cook_baseline(plan, derived_cook_baseline))
}

fn acknowledgement_outcome(
    identity: &SubmissionIdentity,
    submitted: AgentTaskRunRecord,
    record: Option<AgentTaskRunRecord>,
    aggregate: Option<AgentTaskAggregate>,
    exit_code: i32,
    queued: bool,
    outcome: ControlPlaneActionOutcome,
) -> Result<AgentTaskSubmissionOutcome> {
    let record = record.unwrap_or_else(|| submitted.clone());
    let run = RunId::new(&record.run_id).map_err(|error| {
        Error::validation_invalid_argument(
            "run_id",
            error.to_string(),
            Some(record.run_id.clone()),
            None,
        )
    })?;
    let resource = crate::orchestration::project_record(&record, None).map_err(|error| {
        Error::validation_invalid_argument("run", error.message, Some(record.run_id.clone()), None)
    })?;
    let acknowledgement = ControlPlaneSubmissionAcknowledgement {
        schema: CONTROL_PLANE_SUBMISSION_ACKNOWLEDGEMENT_SCHEMA.to_string(),
        acknowledgement: format!("{}:submission:{}", run.as_str(), identity.idempotency_key),
        run,
        idempotency_key: identity.idempotency_key.clone(),
        actor: identity.actor.clone(),
        accepted_at: identity.accepted_at.clone(),
        outcome,
        queued,
        resource,
        message: None,
    };
    Ok(AgentTaskSubmissionOutcome {
        acknowledgement,
        submitted,
        record,
        aggregate,
        exit_code,
    })
}

fn submission_identity(
    request: &ControlPlaneSubmissionRequest,
    plan: &AgentTaskPlan,
    existing: Option<&AgentTaskRunRecord>,
    queued_plan_enrichment: bool,
    claimed_plan_enrichment: bool,
) -> Result<SubmissionIdentity> {
    let Some(existing) = existing else {
        return Ok(SubmissionIdentity {
            idempotency_key: request.idempotency_key.clone(),
            actor: request.actor.clone(),
            accepted_at: agent_task_lifecycle::now_timestamp(),
        });
    };
    let task_ids = plan_task_ids(plan);
    let existing_task_ids = existing
        .tasks
        .iter()
        .map(|task| task.task_id.clone())
        .collect::<Vec<_>>();
    if existing.plan_id != plan.plan_id || existing_task_ids != task_ids {
        return Err(submission_conflict(
            request,
            "run id is already bound to a different plan identity",
        ));
    }
    let Some(metadata) = existing.metadata.get(SUBMISSION_METADATA_KEY) else {
        return Ok(SubmissionIdentity {
            idempotency_key: request.idempotency_key.clone(),
            actor: request.actor.clone(),
            accepted_at: existing.submitted_at.clone(),
        });
    };
    let plan_fingerprint = plan_fingerprint(plan)?;
    let queue_to_run = queued_plan_enrichment
        && existing.state == AgentTaskRunState::Queued
        && !request.queue_only;
    let claimed_queue_to_run = claimed_plan_enrichment
        && existing.state == AgentTaskRunState::Running
        && metadata["queue_only"].as_bool() == Some(true)
        && !request.queue_only;
    let concurrent_queue_completion = queued_plan_enrichment
        && existing.state.is_terminal()
        && metadata["queue_only"].as_bool() == Some(false)
        && !request.queue_only;
    if metadata["schema"] != SUBMISSION_METADATA_SCHEMA
        || metadata["idempotency_key"] != request.idempotency_key
        || metadata["run_id"] != request.run.as_str()
        || metadata["plan_id"] != plan.plan_id
        || metadata["task_ids"] != json!(task_ids)
        || (metadata["plan_sha256"] != plan_fingerprint
            && !queue_to_run
            && !claimed_queue_to_run
            && !concurrent_queue_completion)
    {
        return Err(submission_conflict(
            request,
            "idempotency identity is already bound to different submission input",
        ));
    }
    Ok(SubmissionIdentity {
        idempotency_key: request.idempotency_key.clone(),
        actor: metadata["actor"]
            .as_str()
            .unwrap_or(&request.actor)
            .to_string(),
        accepted_at: metadata["accepted_at"]
            .as_str()
            .unwrap_or(&existing.submitted_at)
            .to_string(),
    })
}

fn submission_metadata(
    request: &ControlPlaneSubmissionRequest,
    plan: &AgentTaskPlan,
    identity: &SubmissionIdentity,
) -> Map<String, Value> {
    let mut metadata = Map::new();
    metadata.insert(
        SUBMISSION_METADATA_KEY.to_string(),
        json!({
            "schema": SUBMISSION_METADATA_SCHEMA,
            "idempotency_key": identity.idempotency_key,
            "run_id": request.run.as_str(),
            "actor": identity.actor,
            "accepted_at": identity.accepted_at,
            "plan_id": plan.plan_id,
            "task_ids": plan_task_ids(plan),
            "plan_sha256": plan_fingerprint(plan).expect("prepared plan is serializable"),
            "queue_only": request.queue_only,
        }),
    );
    metadata
}

fn plan_task_ids(plan: &AgentTaskPlan) -> Vec<String> {
    plan.tasks.iter().map(|task| task.task_id.clone()).collect()
}

fn plan_fingerprint(plan: &AgentTaskPlan) -> Result<String> {
    let value = serde_json::to_value(plan)
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    let bytes = serde_json::to_vec(&canonicalize_json(value))
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    Ok(homeboy_engine_primitives::content_hash::sha256_hex(&bytes))
}

fn canonicalize_json(value: Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .map(|(key, value)| (key, canonicalize_json(value)))
                .collect::<std::collections::BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.into_iter().map(canonicalize_json).collect()),
        value => value,
    }
}

fn submission_conflict(request: &ControlPlaneSubmissionRequest, message: &str) -> Error {
    Error::validation_invalid_argument(
        "idempotency_key",
        message,
        Some(request.idempotency_key.clone()),
        None,
    )
}

fn accepted_runner_handoff_can_materialize(record: &AgentTaskRunRecord) -> bool {
    let Some(runner_id) = agent_task_lifecycle::execution_runner_id() else {
        return false;
    };
    record.runner_id() == Some(runner_id.as_str())
        && record.lab_handoff.as_ref().is_some_and(|handoff| {
            handoff.state == agent_task_lifecycle::AgentTaskLabHandoffState::Accepted
                && handoff.authority
                    == agent_task_lifecycle::AgentTaskLabHandoffAuthority::RunnerDaemon
        })
}

fn with_submission_lock<T>(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    use fs4::fs_std::FileExt;

    let run_dir = lifecycle_store.run_dir(run_id);
    std::fs::create_dir_all(&run_dir).map_err(|error| {
        Error::internal_io(error.to_string(), Some(run_dir.display().to_string()))
    })?;
    let lock_path = run_dir.join("submission.lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|error| {
            Error::internal_io(error.to_string(), Some(lock_path.display().to_string()))
        })?;
    lock.lock_exclusive().map_err(|error| {
        Error::internal_io(error.to_string(), Some(lock_path.display().to_string()))
    })?;
    let result = operation();
    let unlock = FileExt::unlock(&lock).map_err(|error| {
        Error::internal_io(error.to_string(), Some(lock_path.display().to_string()))
    });
    match (result, unlock) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_task::{
        AgentTaskExecutor, AgentTaskLimits, AgentTaskOutcome, AgentTaskOutcomeStatus,
        AgentTaskPolicy, AgentTaskRequest, AgentTaskWorkspace, AGENT_TASK_REQUEST_SCHEMA,
    };
    use crate::agent_task_scheduler::{AgentTaskExecutionContext, AgentTaskExecutorAdapter};
    use homeboy_core::test_support::with_isolated_home;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};

    #[derive(Clone)]
    struct CountingExecutor {
        calls: Arc<AtomicUsize>,
        submitted_observed: Option<Arc<AtomicBool>>,
    }

    impl AgentTaskExecutorAdapter for CountingExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            if let Some(observed) = self.submitted_observed.as_ref() {
                assert!(observed.load(Ordering::SeqCst));
            }
            self.calls.fetch_add(1, Ordering::SeqCst);
            AgentTaskOutcome {
                task_id: request.task_id,
                status: AgentTaskOutcomeStatus::Succeeded,
                ..Default::default()
            }
        }
    }

    #[test]
    fn terminal_replay_returns_stable_acknowledgement_without_executing_twice() {
        with_isolated_home(|_| {
            let calls = Arc::new(AtomicUsize::new(0));
            let request = prepared_submission_request(
                Some("submission-terminal-replay"),
                false,
                "first-actor",
            )
            .expect("request");
            let executor = Arc::new(CountingExecutor {
                calls: Arc::clone(&calls),
                submitted_observed: None,
            });

            let first = submit_prepared_plan(
                &request,
                PreparedAgentTaskSubmission::new(test_plan("submission-plan")),
                executor.clone(),
            )
            .expect("first submission");
            let replay = submit_prepared_plan(
                &request,
                PreparedAgentTaskSubmission::new(test_plan("submission-plan")),
                executor,
            )
            .expect("terminal replay");

            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(
                replay.acknowledgement.outcome,
                ControlPlaneActionOutcome::AlreadySatisfied
            );
            assert_eq!(
                replay.acknowledgement.accepted_at,
                first.acknowledgement.accepted_at
            );
            assert_eq!(
                replay.acknowledgement.acknowledgement,
                first.acknowledgement.acknowledgement
            );
        });
    }

    #[test]
    fn queued_submission_executes_once_and_preserves_original_actor() {
        with_isolated_home(|_| {
            let calls = Arc::new(AtomicUsize::new(0));
            let queued_request =
                prepared_submission_request(Some("submission-queue-run"), true, "queue-actor")
                    .expect("queue request");
            let executor = Arc::new(CountingExecutor {
                calls: Arc::clone(&calls),
                submitted_observed: None,
            });
            let queued = submit_prepared_plan(
                &queued_request,
                PreparedAgentTaskSubmission::new(test_plan("queue-plan")),
                executor.clone(),
            )
            .expect("queued submission");
            assert_eq!(calls.load(Ordering::SeqCst), 0);

            let run_request =
                prepared_submission_request(Some("submission-queue-run"), false, "runner-actor")
                    .expect("run request");
            let completed = submit_prepared_plan(
                &run_request,
                PreparedAgentTaskSubmission::new(test_plan("queue-plan")),
                executor.clone(),
            )
            .expect("execute queued submission");
            let replay = submit_prepared_plan(
                &run_request,
                PreparedAgentTaskSubmission::new(test_plan("queue-plan")),
                executor,
            )
            .expect("completed replay");

            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(completed.acknowledgement.actor, "queue-actor");
            assert_eq!(
                completed.acknowledgement.accepted_at,
                queued.acknowledgement.accepted_at
            );
            assert_eq!(
                replay.acknowledgement.outcome,
                ControlPlaneActionOutcome::AlreadySatisfied
            );
        });
    }

    #[test]
    fn staged_admission_survives_preparation_before_execution() {
        with_isolated_home(|_| {
            let run_id = "submission-staged-admission";
            let queued_request =
                prepared_submission_request(Some(run_id), true, "queue-actor").expect("queue");
            queue_prepared_plan(
                &queued_request,
                PreparedAgentTaskSubmission::new(test_plan("staged-plan")),
            )
            .expect("queued");

            let request =
                prepared_submission_request(Some(run_id), false, "run-actor").expect("run");
            let mut admitted = test_plan("staged-plan");
            admitted.metadata["selected_provider"] = json!("fallback");
            stage_prepared_plan(
                &request,
                PreparedAgentTaskSubmission::new(admitted.clone()).with_queued_plan_enrichment(),
            )
            .expect("stage admitted plan");
            assert_eq!(
                agent_task_lifecycle::load_plan(run_id)
                    .expect("staged plan")
                    .metadata["selected_provider"],
                "fallback"
            );

            admitted.metadata["prepared_workspace"] = json!(true);
            let calls = Arc::new(AtomicUsize::new(0));
            let outcome = submit_prepared_plan(
                &request,
                PreparedAgentTaskSubmission::new(admitted).with_queued_plan_enrichment(),
                Arc::new(CountingExecutor {
                    calls: Arc::clone(&calls),
                    submitted_observed: None,
                }),
            )
            .expect("execute prepared plan");

            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(outcome.record.state, AgentTaskRunState::Succeeded);
        });
    }

    #[test]
    fn live_running_replay_does_not_repersist_or_execute() {
        with_isolated_home(|_| {
            let calls = Arc::new(AtomicUsize::new(0));
            let queued_request =
                prepared_submission_request(Some("submission-running-replay"), true, "queue-actor")
                    .expect("queue request");
            let executor = Arc::new(CountingExecutor {
                calls: Arc::clone(&calls),
                submitted_observed: None,
            });
            submit_prepared_plan(
                &queued_request,
                PreparedAgentTaskSubmission::new(test_plan("running-plan")),
                executor.clone(),
            )
            .expect("queued submission");
            agent_task_lifecycle::mark_running("submission-running-replay").expect("mark running");

            let run_request = prepared_submission_request(
                Some("submission-running-replay"),
                false,
                "runner-actor",
            )
            .expect("run request");
            let error = submit_prepared_plan(
                &run_request,
                PreparedAgentTaskSubmission::new(test_plan("running-plan")),
                executor,
            )
            .expect_err("running replay must fail");

            assert!(error.message.contains("already running"));
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert_eq!(
                agent_task_lifecycle::status("submission-running-replay")
                    .expect("record")
                    .state,
                AgentTaskRunState::Running
            );
        });
    }

    #[test]
    fn submission_observer_runs_after_persistence_and_before_execution() {
        with_isolated_home(|_| {
            let calls = Arc::new(AtomicUsize::new(0));
            let observed = Arc::new(AtomicBool::new(false));
            let executor = Arc::new(CountingExecutor {
                calls: Arc::clone(&calls),
                submitted_observed: Some(Arc::clone(&observed)),
            });
            let request =
                prepared_submission_request(Some("submission-observer-order"), false, "controller")
                    .expect("request");

            submit_prepared_plan_with_observer(
                &request,
                PreparedAgentTaskSubmission::new(test_plan("observer-plan")),
                executor,
                |submitted| {
                    assert_eq!(submitted.state, AgentTaskRunState::Queued);
                    assert!(agent_task_lifecycle::run_record_exists(&submitted.run_id)?);
                    observed.store(true, Ordering::SeqCst);
                    Ok(())
                },
            )
            .expect("observed submission");

            assert_eq!(calls.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn run_id_cannot_be_reused_for_a_different_plan() {
        with_isolated_home(|_| {
            let request =
                prepared_submission_request(Some("submission-plan-conflict"), true, "actor")
                    .expect("request");
            let executor = Arc::new(CountingExecutor {
                calls: Arc::new(AtomicUsize::new(0)),
                submitted_observed: None,
            });
            submit_prepared_plan(
                &request,
                PreparedAgentTaskSubmission::new(test_plan("first-plan")),
                executor.clone(),
            )
            .expect("first submission");

            let error = submit_prepared_plan(
                &request,
                PreparedAgentTaskSubmission::new(test_plan("other-plan")),
                executor,
            )
            .expect_err("plan collision must fail");
            assert!(error.message.contains("different plan identity"));
        });
    }

    #[test]
    fn queue_to_run_rejects_untrusted_plan_replacement() {
        with_isolated_home(|_| {
            let queued_request =
                prepared_submission_request(Some("submission-queue-replacement"), true, "actor")
                    .expect("queue request");
            let executor = Arc::new(CountingExecutor {
                calls: Arc::new(AtomicUsize::new(0)),
                submitted_observed: None,
            });
            submit_prepared_plan(
                &queued_request,
                PreparedAgentTaskSubmission::new(test_plan("queue-replacement-plan")),
                executor.clone(),
            )
            .expect("queued submission");

            let run_request =
                prepared_submission_request(Some("submission-queue-replacement"), false, "actor")
                    .expect("run request");
            let mut replacement = test_plan("queue-replacement-plan");
            replacement.tasks[0].instructions = "different work".to_string();
            let error = submit_prepared_plan(
                &run_request,
                PreparedAgentTaskSubmission::new(replacement),
                executor,
            )
            .expect_err("untrusted replacement must fail");

            assert!(error.message.contains("different submission input"));
        });
    }

    #[test]
    fn stale_queued_enrichment_replays_a_concurrently_completed_run() {
        with_isolated_home(|_| {
            let calls = Arc::new(AtomicUsize::new(0));
            let executor = Arc::new(CountingExecutor {
                calls: Arc::clone(&calls),
                submitted_observed: None,
            });
            let queued_request = prepared_submission_request(
                Some("submission-concurrent-queue-completion"),
                true,
                "queue-actor",
            )
            .expect("queue request");
            submit_prepared_plan(
                &queued_request,
                PreparedAgentTaskSubmission::new(test_plan("queue-completion-plan")),
                executor.clone(),
            )
            .expect("queued submission");

            let run_request = prepared_submission_request(
                Some("submission-concurrent-queue-completion"),
                false,
                "runner-actor",
            )
            .expect("run request");
            let mut selected = test_plan("queue-completion-plan");
            selected.tasks[0].executor.model = Some("selected-model".to_string());
            submit_prepared_plan(
                &run_request,
                PreparedAgentTaskSubmission::new(selected).with_queued_plan_enrichment(),
                executor.clone(),
            )
            .expect("first queued consumer completes");

            let mut stale = test_plan("queue-completion-plan");
            stale.tasks[0].executor.model = Some("stale-model".to_string());
            let replay = submit_prepared_plan(
                &run_request,
                PreparedAgentTaskSubmission::new(stale).with_queued_plan_enrichment(),
                executor,
            )
            .expect("stale queued consumer replays terminal evidence");

            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(
                replay.acknowledgement.outcome,
                ControlPlaneActionOutcome::AlreadySatisfied
            );
        });
    }

    #[test]
    fn fingerprint_canonicalizes_nested_object_keys() {
        let left = json!({ "outer": { "z": 1, "a": 2 }, "b": 3 });
        let right = json!({ "b": 3, "outer": { "a": 2, "z": 1 } });
        assert_eq!(canonicalize_json(left), canonicalize_json(right));
    }

    #[test]
    fn concurrent_conflicting_submissions_execute_one_bound_plan() {
        with_isolated_home(|_| {
            let calls = Arc::new(AtomicUsize::new(0));
            let barrier = Arc::new(Barrier::new(2));
            let mut threads = Vec::new();
            for plan_id in ["concurrent-plan-a", "concurrent-plan-b"] {
                let calls = Arc::clone(&calls);
                let barrier = Arc::clone(&barrier);
                threads.push(std::thread::spawn(move || {
                    let request = prepared_submission_request(
                        Some("submission-concurrent-conflict"),
                        false,
                        "actor",
                    )?;
                    barrier.wait();
                    submit_prepared_plan(
                        &request,
                        PreparedAgentTaskSubmission::new(test_plan(plan_id)),
                        Arc::new(CountingExecutor {
                            calls,
                            submitted_observed: None,
                        }),
                    )
                }));
            }

            let results = threads
                .into_iter()
                .map(|thread| thread.join().expect("submission thread"))
                .collect::<Vec<_>>();
            assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
            assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            let record = agent_task_lifecycle::status("submission-concurrent-conflict")
                .expect("durable record");
            assert!(matches!(
                record.plan_id.as_str(),
                "concurrent-plan-a" | "concurrent-plan-b"
            ));
        });
    }

    fn test_plan(plan_id: &str) -> AgentTaskPlan {
        AgentTaskPlan::new(
            plan_id,
            vec![AgentTaskRequest {
                schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
                task_id: "submission-task".to_string(),
                group_key: None,
                parent_plan_id: None,
                executor: AgentTaskExecutor {
                    backend: "test".to_string(),
                    selector: Some("submission".to_string()),
                    runtime_selection: None,
                    required_capabilities: Vec::new(),
                    secret_env: Vec::new(),
                    model: None,
                    config: Value::Null,
                },
                instructions: "run".to_string(),
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
                metadata: Value::Null,
            }],
        )
    }
}
