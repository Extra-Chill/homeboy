//! Public Homeboy orchestration service.
//!
//! Owns capabilities and run retrieval. The local CLI and the daemon HTTP
//! adapter call this service; they do not assemble a second control-plane
//! projection. Construct with an explicit lookup — the service does not
//! resolve ambient stores or providers itself.

use chrono::{DateTime, Utc};
use homeboy_control_plane_contract::ControlPlaneRetryParameters;
use homeboy_control_plane_contract::{
    ControlPlaneAction, ControlPlaneActionAcknowledgement, ControlPlaneActionOutcome,
    ControlPlaneActionPayload, ControlPlaneActionRequest, ControlPlaneBlocker,
    ControlPlaneCancelDisposition, ControlPlaneCancelParameters, ControlPlaneCancelResult,
    ControlPlaneCapabilities, ControlPlaneError, ControlPlaneErrorClass, ControlPlaneEvidenceRef,
    ControlPlaneLiveness, ControlPlaneLocation, ControlPlaneOperation, ControlPlaneOwner,
    ControlPlaneProviderSummary, ControlPlaneResource, ControlPlaneRun, ControlPlaneRunReview,
    ControlPlaneRunReviewRequest, ControlPlaneRunState, ControlPlaneRuntime,
    ControlPlaneStateSummary, ExecutionId, ProviderSessionId, RunId,
    CONTROL_PLANE_ACTION_ACKNOWLEDGEMENT_SCHEMA, CONTROL_PLANE_ACTION_REQUEST_SCHEMA,
    CONTROL_PLANE_CANCEL_PARAMETERS_SCHEMA, CONTROL_PLANE_CANCEL_RESULT_SCHEMA,
    CONTROL_PLANE_EMPTY_ACTION_PAYLOAD_SCHEMA, CONTROL_PLANE_PROMOTE_PARAMETERS_SCHEMA,
    CONTROL_PLANE_PROMOTE_RESULT_SCHEMA, CONTROL_PLANE_RESUME_RESULT_SCHEMA,
    CONTROL_PLANE_RETRY_PARAMETERS_SCHEMA, CONTROL_PLANE_RETRY_RESULT_SCHEMA,
};
use homeboy_core::control_plane::{register_control_plane_provider, ControlPlaneProvider};
use serde_json::Value;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::agent_task_lifecycle::{
    canonical_control_plane_identities, claim_operation_with_intent_in_store,
    complete_cook_operation_in_store, lifecycle_action_eligibility, now_timestamp,
    resolve_run_id_in_store, AgentTaskLifecycleStore, AgentTaskRunRecord, AgentTaskRunState,
    CanonicalControlPlaneIdentities, ClaimOutcome,
};
use crate::agent_task_schedule::AgentTaskPlan;

const ID_BOUND: usize = 128;
const STATE_BOUND: usize = 64;
const MESSAGE_BOUND: usize = 256;
const GATE_BOUND: usize = 12;
pub(crate) const REF_BOUND: usize = 32;
const URI_BOUND: usize = 512;
const EVENT_PAGE_BOUND: usize = 100;
const ACTION_INPUT_BOUND: usize = 128;
const ACTION_REASON_BOUND: usize = 1_024;
const REVIEW_EVIDENCE_BOUND: usize = 1024 * 1024;
const REVIEW_EVIDENCE_FIELD_BOUND: usize = 256 * 1024;
const ACTION_LEASE: std::time::Duration = std::time::Duration::from_secs(30);
const CANCEL_TERMINAL_WAIT: Duration = Duration::from_secs(15);
const CANCEL_TERMINAL_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// One bounded non-reconciling read of the durable record and optional plan.
#[derive(Debug, Clone)]
pub struct RunSnapshot {
    pub record: AgentTaskRunRecord,
    pub plan: Option<AgentTaskPlan>,
}

/// Lookup used by [`OrchestrationService`]. Callers inject stores or test
/// doubles; the service never opens an environment-rooted store itself.
pub trait RunLookup {
    fn get(&self, id: &RunId) -> Result<Option<RunSnapshot>, ControlPlaneError>;
}

pub trait EventLookup {
    fn events(
        &self,
        id: &RunId,
        cursor: Option<&homeboy_control_plane_contract::EventCursor>,
    ) -> Result<Option<homeboy_control_plane_contract::ControlPlaneEventPage>, ControlPlaneError>;
}

/// Durable lifecycle-store lookup. Bounded, non-reconciling, non-writing.
pub struct LifecycleStoreLookup {
    store: AgentTaskLifecycleStore,
}

impl LifecycleStoreLookup {
    pub fn new(store: AgentTaskLifecycleStore) -> Self {
        Self { store }
    }
}

impl RunLookup for LifecycleStoreLookup {
    fn get(&self, id: &RunId) -> Result<Option<RunSnapshot>, ControlPlaneError> {
        let record = match self.store.read_record_bounded(id.as_str()) {
            Ok(record) => record,
            Err(error) if is_run_not_found(&error) => return Ok(None),
            Err(error) => return Err(ControlPlaneError::unavailable(error.message)),
        };
        let plan = match self.store.read_controller_plan(&record.run_id) {
            Ok(plan) => Some(plan),
            Err(error)
                if error.code == homeboy_core::ErrorCode::ValidationInvalidArgument
                    && error
                        .message
                        .contains("unsupported agent-task execution budget version") =>
            {
                return Err(ControlPlaneError::invalid_argument(error.message));
            }
            Err(_) => None,
        };
        Ok(Some(RunSnapshot { record, plan }))
    }
}

impl EventLookup for LifecycleStoreLookup {
    fn events(
        &self,
        id: &RunId,
        cursor: Option<&homeboy_control_plane_contract::EventCursor>,
    ) -> Result<Option<homeboy_control_plane_contract::ControlPlaneEventPage>, ControlPlaneError>
    {
        match crate::agent_task_lifecycle::control_plane_events_in_store(
            &self.store,
            id.as_str(),
            cursor,
        ) {
            Ok(events) => Ok(Some(events)),
            Err(error) if is_run_not_found(&error) => Ok(None),
            Err(error) => Err(ControlPlaneError::unavailable(error.message)),
        }
    }
}

/// Public typed orchestration facade.
pub struct OrchestrationService<L> {
    lookup: L,
}

impl<L: RunLookup> OrchestrationService<L> {
    pub fn new(lookup: L) -> Self {
        Self { lookup }
    }

    /// Operations available to a read-only injected lookup.
    pub fn read_capabilities() -> ControlPlaneCapabilities {
        ControlPlaneCapabilities::new(
            vec![ControlPlaneResource::Run, ControlPlaneResource::Event],
            vec![
                ControlPlaneOperation::GetCapabilities,
                ControlPlaneOperation::GetRun,
                ControlPlaneOperation::GetRunEvents,
            ],
        )
    }

    /// Pure, bounded, non-reconciling run read.
    pub fn run(&self, requested_id: &RunId) -> Result<ControlPlaneRun, ControlPlaneError> {
        let snapshot = self.lookup.get(requested_id)?.ok_or_else(|| {
            ControlPlaneError::not_found(format!("agent-task run not found: {requested_id}"))
        })?;
        project_record(&snapshot.record, snapshot.plan.as_ref())
    }
}

fn review_promotion_candidates(
    run_id: &str,
    request: &ControlPlaneRunReviewRequest,
    aggregate: &crate::agent_tasks::AgentTaskAggregate,
    review: &crate::agent_tasks::AgentTaskAggregateReport,
    record: &AgentTaskRunRecord,
    cook_contract: Option<&(String, Value)>,
    observation_store: &homeboy_core::observation::ObservationStore,
) -> Vec<Value> {
    review
        .apply_candidates
        .iter()
        .chain(review.review_candidates.iter().filter(|candidate| {
            aggregate
                .outcomes
                .iter()
                .find(|outcome| outcome.task_id == candidate.task_id)
                .is_some_and(|outcome| {
                    outcome.status
                        == crate::agent_tasks::AgentTaskOutcomeStatus::CandidateRecoverable
                })
        }))
        .flat_map(|candidate| {
            let artifact_ids = aggregate
                .outcomes
                .iter()
                .find(|outcome| outcome.task_id == candidate.task_id)
                .filter(|outcome| {
                    outcome.status
                        == crate::agent_tasks::AgentTaskOutcomeStatus::CandidateRecoverable
                })
                .and_then(|outcome| {
                    crate::agent_task_promotion::canonical_recoverable_patch_artifacts_in_observation_store(
                        outcome,
                        &crate::agent_task_promotion::AgentTaskPromotionOptions {
                            source: "{}".to_string(),
                            source_run_id: Some(run_id.to_string()),
                            source_path: None,
                            source_worktree_path: None,
                            base_ref: None,
                            task_base_sha: None,
                            candidate_ref: None,
                            to_worktree: "<managed-worktree>".to_string(),
                            task_id: Some(candidate.task_id.clone()),
                            artifact_id: None,
                            dry_run: true,
                            gates: Default::default(),
                            provider_command: None,
                            provider_invocation: None,
                        },
                        observation_store,
                    )
                    .ok()
                    .map(|canonical| {
                        canonical
                            .artifacts
                            .into_iter()
                            .map(|artifact| artifact.id)
                            .collect::<Vec<_>>()
                    })
                })
                .unwrap_or_else(|| candidate.artifact_ids.clone());
            let selection_required = artifact_ids.len() > 1;
            artifact_ids.into_iter().map(move |artifact_id| {
                let mut command = vec![
                    "homeboy".to_string(),
                    "agent-task".to_string(),
                    "promote".to_string(),
                    run_id.to_string(),
                    "--task-id".to_string(),
                    candidate.task_id.clone(),
                    "--artifact-id".to_string(),
                    artifact_id.clone(),
                ];
                let continuation = record.metadata.get("latest_promotion").filter(|promotion| {
                    crate::agent_task_service::promotion_is_resumable(promotion, false)
                        && promotion.pointer("/source/task_id").and_then(Value::as_str)
                            == Some(candidate.task_id.as_str())
                        && promotion
                            .pointer("/patch_artifact/id")
                            .and_then(Value::as_str)
                            == Some(artifact_id.as_str())
                });
                let destination = continuation
                    .and_then(|promotion| promotion.pointer("/target/worktree"))
                    .and_then(Value::as_str)
                    .or(request.to_worktree.as_deref());
                let command = destination.and_then(|destination| {
                    command.extend(["--to-worktree".to_string(), destination.to_string()]);
                    let resume_contract = continuation
                        .and_then(|promotion| promotion.pointer("/provenance/resume_contract"))
                        .filter(|_| cook_contract.is_none());
                    let resume_gate_error = resume_contract.and_then(|contract| {
                        contract
                            .get("gates")
                            .ok_or("resume contract has no gate policy")
                            .and_then(|gates| {
                                serde_json::from_value::<crate::agent_task_gate::VerifyGateOptions>(
                                    gates.clone(),
                                )
                                .map(|_| ())
                                .map_err(|_| "resume contract has an invalid gate policy")
                            })
                            .err()
                    });
                    if let Some(contract) = continuation
                        .and_then(|promotion| promotion.pointer("/provenance/resume_contract"))
                    {
                        append_resume_base(&mut command, contract);
                        if resume_contract.is_some() && resume_gate_error.is_none() {
                            command.push("--gates-from-resume-contract".to_string());
                        }
                    } else if let Some((base, _)) = cook_contract {
                        command.extend(["--base".to_string(), base.clone()]);
                    }
                    if cook_contract.is_some() {
                        command.push("--gates-from-cook-recipe".to_string());
                    }
                    if let Some(provider_command) = &request.provider_command {
                        command
                            .extend(["--provider-command".to_string(), provider_command.clone()]);
                    }
                    command.extend(
                        request
                            .provider_argv
                            .iter()
                            .map(|argument| format!("--provider-argv={argument}")),
                    );
                    resume_gate_error.is_none().then_some(command)
                });
                serde_json::json!({
                    "task_id": candidate.task_id,
                    "artifact_id": artifact_id,
                    "reason": candidate.reason,
                    "command": command,
                    "ready": command.is_some(),
                    "destination_required": destination.is_none(),
                    "selection_required": selection_required,
                    "unavailable_reason": (destination.is_some() && command.is_none()).then_some("durable resume contract has an invalid gate policy"),
                })
            })
        })
        .collect()
}

fn append_resume_base(command: &mut Vec<String>, contract: &Value) {
    if let Some(base) = contract.pointer("/inputs/base_ref").and_then(Value::as_str) {
        command.extend(["--base".to_string(), base.to_string()]);
    }
}

fn review_retry_context(run_id: &str) -> Value {
    serde_json::json!({
        "run_id": run_id,
        "retry_action": ControlPlaneAction::Retry,
        "resume_action": ControlPlaneAction::Resume,
    })
}

impl OrchestrationService<LifecycleStoreLookup> {
    /// Operations wired by the durable lifecycle-backed provider.
    pub fn capabilities() -> ControlPlaneCapabilities {
        let mut capabilities = Self::read_capabilities();
        capabilities.resources.push(ControlPlaneResource::Review);
        capabilities
            .operations
            .push(ControlPlaneOperation::GetRunReview);
        capabilities
            .operations
            .push(ControlPlaneOperation::ExecuteRunAction);
        capabilities
    }

    /// Read review evidence exclusively from this service's injected store.
    pub fn review(
        &self,
        requested_id: &RunId,
        request: &ControlPlaneRunReviewRequest,
    ) -> Result<ControlPlaneRunReview, ControlPlaneError> {
        request.validate()?;
        let snapshot = self.lookup.get(requested_id)?.ok_or_else(|| {
            ControlPlaneError::not_found(format!("agent-task run not found: {requested_id}"))
        })?;
        let durable_read = crate::agent_task_lifecycle::durable_local_read_in_store(
            &self.lookup.store,
            requested_id.as_str(),
        )
        .map_err(map_lifecycle_error)?;
        let resource = project_record(&durable_read.record, snapshot.plan.as_ref())?;
        let aggregate = durable_read.aggregate;
        let aggregate_review = aggregate.as_ref().map(|aggregate| {
            crate::agent_tasks::AgentTaskAggregateReport::from(aggregate.outcomes.clone())
        });
        let cook_contract = review_cook_contract(&self.lookup.store, requested_id.as_str())?;
        let observation_store =
            homeboy_core::observation::ObservationStore::open_initialized_for_lifecycle_in_roots(
                self.lookup.store.roots(),
            )
            .map_err(map_lifecycle_error)?;
        let promotion_candidates = aggregate
            .as_ref()
            .zip(aggregate_review.as_ref())
            .map(|(aggregate, review)| {
                review_promotion_candidates(
                    requested_id.as_str(),
                    request,
                    aggregate,
                    review,
                    &durable_read.record,
                    cook_contract.as_ref(),
                    &observation_store,
                )
            })
            .unwrap_or_default();
        let failure_reasons = aggregate
            .as_ref()
            .map(review_failure_reasons)
            .filter(|reasons| !reasons.is_empty());
        let canonical_candidate =
            review_canonical_candidate(&durable_read.record, aggregate_review.as_ref(), &resource);
        let (record, cleanup_evidence) = review_record_projection(&durable_read.record);
        let evidence = bounded_review_evidence(serde_json::json!({
            "record": record,
            "logs": crate::agent_task_lifecycle::logs_in_store(&self.lookup.store, requested_id.as_str()).map_err(map_lifecycle_error)?,
            "artifacts": crate::agent_task_lifecycle::artifacts_in_store(&self.lookup.store, requested_id.as_str()).map_err(map_lifecycle_error)?,
            "aggregate": aggregate,
            "aggregate_review": aggregate_review,
            "promotion_candidates": promotion_candidates,
            "diagnostic_summary": failure_reasons.as_ref().and_then(|reasons| reasons.first()).cloned(),
            "failure_reasons": failure_reasons,
            "execution_states": review_execution_states(aggregate.as_ref(), &durable_read.record, &resource),
            "canonical_candidate": canonical_candidate,
            "next_actions": review_next_actions(&durable_read.record, aggregate_review.as_ref(), request.to_worktree.is_some()),
            "cleanup_evidence": cleanup_evidence,
            "transport": { "authoritative": "homeboy-agent-task-lifecycle", "chat_state_required": false },
            "action_eligibility": resource.action_eligibility,
            "retry_context": review_retry_context(requested_id.as_str()),
            "read": { "phase": "controller_local", "mutated": false, "unavailable_sources": durable_read.unavailable_sources },
        }));
        Ok(ControlPlaneRunReview {
            schema: homeboy_control_plane_contract::CONTROL_PLANE_RUN_REVIEW_SCHEMA.to_string(),
            run: requested_id.clone(),
            resource,
            evidence,
        })
    }

    /// Execute the canonical run mutation against the same durable lifecycle
    /// store used by status and events.
    pub fn execute_action(
        &self,
        requested_id: &RunId,
        request: &ControlPlaneActionRequest,
    ) -> Result<ControlPlaneActionAcknowledgement, ControlPlaneError> {
        self.execute_action_with_delegates(
            requested_id,
            request,
            |parameters| default_retry(requested_id.as_str(), parameters),
            || default_resume(requested_id.as_str()),
            default_promote,
        )
    }

    fn execute_action_with_delegates<F, R, P>(
        &self,
        requested_id: &RunId,
        request: &ControlPlaneActionRequest,
        retry: F,
        resume: R,
        promote: P,
    ) -> Result<ControlPlaneActionAcknowledgement, ControlPlaneError>
    where
        F: FnOnce(
            &ControlPlaneRetryParameters,
        )
            -> homeboy_core::Result<crate::agent_task_service::AgentTaskRetryServiceResult>,
        R: FnOnce() -> homeboy_core::Result<
            crate::agent_task_service::AgentTaskRunResult<
                crate::agent_task_schedule::AgentTaskAggregate,
            >,
        >,
        P: FnOnce(
            &crate::agent_task_service::AgentTaskPromotionRequest,
        )
            -> homeboy_core::Result<crate::agent_task_promotion::AgentTaskPromotionReport>,
    {
        validate_action_request(request)?;
        let resolved = resolve_run_id_in_store(&self.lookup.store, requested_id.as_str())
            .map_err(map_lifecycle_error)?;
        let record = self
            .lookup
            .store
            .read_record(&resolved)
            .map_err(map_lifecycle_error)?;
        let operation_key = format!(
            "control-plane-action:{}:{}",
            action_name(request.action),
            request.idempotency_key
        );
        let intent = serde_json::to_value(request)
            .map_err(|error| ControlPlaneError::invalid_argument(error.to_string()))?;
        match claim_operation_with_intent_in_store(
            &self.lookup.store,
            &resolved,
            &operation_key,
            ACTION_LEASE,
            &intent,
        )
        .map_err(map_lifecycle_error)?
        {
            ClaimOutcome::AlreadyCompleted(result) => {
                serde_json::from_value(result).map_err(|error| {
                    ControlPlaneError::unavailable(format!("stored action result: {error}"))
                })
            }
            ClaimOutcome::LeaseHeld => Err(ControlPlaneError::unavailable(
                "this idempotent action is already in progress",
            )),
            ClaimOutcome::Acquired => {
                let accepted_at = now_timestamp();
                let acknowledgement = format!(
                    "{}:action:{}:{}",
                    record.run_id,
                    action_name(request.action),
                    request.idempotency_key
                );
                let (outcome, resource, result, message) = if request
                    .expected_updated_at
                    .as_ref()
                    .is_some_and(|expected| record.updated_at.as_ref() != Some(expected))
                {
                    (
                        ControlPlaneActionOutcome::Failed,
                        project_record(&record, None)?,
                        ControlPlaneActionPayload::empty(),
                        Some("run changed since the supplied precondition".to_string()),
                    )
                } else {
                    match request.action {
                        ControlPlaneAction::Cancel if record.state.is_terminal() => (
                            ControlPlaneActionOutcome::AlreadySatisfied,
                            project_record(&record, None)?,
                            cancel_result_payload(cancel_result_for_record(
                                &record,
                                Duration::ZERO,
                                0,
                                None,
                            )),
                            Some("run is already terminal".to_string()),
                        ),
                        ControlPlaneAction::Cancel => {
                            let parameters: ControlPlaneCancelParameters = serde_json::from_value(
                                request.parameters.data.clone(),
                            )
                            .map_err(|error| {
                                ControlPlaneError::invalid_argument(format!(
                                    "cancel parameters: {error}"
                                ))
                            })?;
                            match crate::agent_task_lifecycle::cancel_run_in_store(
                                &self.lookup.store,
                                requested_id.as_str(),
                                parameters.reason.as_deref(),
                            ) {
                                Ok(cancelled) => {
                                    let (observed, result) = self.converge_cancellation(&cancelled);
                                    (
                                        ControlPlaneActionOutcome::Succeeded,
                                        project_record(&observed, None)?,
                                        cancel_result_payload(result),
                                        None,
                                    )
                                }
                                Err(error) => (
                                    ControlPlaneActionOutcome::Failed,
                                    project_record(&record, None)?,
                                    ControlPlaneActionPayload::empty(),
                                    Some(redacted_bounded(&error.message, MESSAGE_BOUND)),
                                ),
                            }
                        }
                        ControlPlaneAction::Reconcile => {
                            match crate::agent_task_service::reconcile_run_in_store(
                                &self.lookup.store,
                                requested_id.as_str(),
                                false,
                            ) {
                                Ok(report) => {
                                    let current = self
                                        .lookup
                                        .store
                                        .read_record(&resolved)
                                        .map_err(map_lifecycle_error)?;
                                    let outcome = if report.failed > 0 {
                                        ControlPlaneActionOutcome::Failed
                                    } else if report.reconciled == 0 {
                                        ControlPlaneActionOutcome::AlreadySatisfied
                                    } else {
                                        ControlPlaneActionOutcome::Succeeded
                                    };
                                    (
                                        outcome,
                                        project_record(&current, None)?,
                                        ControlPlaneActionPayload {
                                            schema: report.schema.to_string(),
                                            data: serde_json::to_value(report).unwrap_or_default(),
                                        },
                                        None,
                                    )
                                }
                                Err(error) => (
                                    ControlPlaneActionOutcome::Failed,
                                    project_record(&record, None)?,
                                    ControlPlaneActionPayload::empty(),
                                    Some(redacted_bounded(&error.message, MESSAGE_BOUND)),
                                ),
                            }
                        }
                        ControlPlaneAction::Retry => {
                            let mut parameters: ControlPlaneRetryParameters =
                                serde_json::from_value(request.parameters.data.clone()).map_err(
                                    |error| {
                                        ControlPlaneError::invalid_argument(format!(
                                            "retry parameters: {error}"
                                        ))
                                    },
                                )?;
                            if parameters.new_run_id.is_none() {
                                let identity = uuid::Uuid::new_v5(
                                    &uuid::Uuid::NAMESPACE_OID,
                                    format!("{}:retry:{}", record.run_id, request.idempotency_key)
                                        .as_bytes(),
                                );
                                parameters.new_run_id = Some(format!("retry-{identity}"));
                            }
                            match retry(&parameters) {
                                Ok(retry) => {
                                    let outcome = if retry.created {
                                        ControlPlaneActionOutcome::Succeeded
                                    } else {
                                        ControlPlaneActionOutcome::AlreadySatisfied
                                    };
                                    (
                                        outcome,
                                        project_record(&retry.record, None)?,
                                        ControlPlaneActionPayload {
                                            schema: CONTROL_PLANE_RETRY_RESULT_SCHEMA.to_string(),
                                            data: serde_json::json!({
                                                "record": retry.record,
                                                "runnable": retry.run,
                                                "created": retry.created,
                                            }),
                                        },
                                        None,
                                    )
                                }
                                Err(error) => (
                                    ControlPlaneActionOutcome::Failed,
                                    project_record(&record, None)?,
                                    ControlPlaneActionPayload::empty(),
                                    Some(redacted_bounded(&error.message, MESSAGE_BOUND)),
                                ),
                            }
                        }
                        ControlPlaneAction::Promote => {
                            let parameters: crate::agent_task_service::AgentTaskPromotionRequest =
                                serde_json::from_value(request.parameters.data.clone()).map_err(
                                    |error| {
                                        ControlPlaneError::invalid_argument(format!(
                                            "promote parameters: {error}"
                                        ))
                                    },
                                )?;
                            match promote(&parameters) {
                                Ok(report) => {
                                    let current = self
                                        .lookup
                                        .store
                                        .read_record(&resolved)
                                        .map_err(map_lifecycle_error)?;
                                    let mut result =
                                        serde_json::to_value(&report).unwrap_or_default();
                                    result["handoff"] = promotion_handoff(&report);
                                    if !parameters.dry_run {
                                        result["recorded_on_run"] = serde_json::json!({
                                            "run_id": current.run_id,
                                            "metadata_key": "latest_promotion",
                                        });
                                    }
                                    (
                                        ControlPlaneActionOutcome::Succeeded,
                                        project_record(&current, None)?,
                                        ControlPlaneActionPayload {
                                            schema: CONTROL_PLANE_PROMOTE_RESULT_SCHEMA.to_string(),
                                            data: result,
                                        },
                                        None,
                                    )
                                }
                                Err(error) => (
                                    ControlPlaneActionOutcome::Failed,
                                    project_record(&record, None)?,
                                    ControlPlaneActionPayload::empty(),
                                    Some(redacted_bounded(&error.message, MESSAGE_BOUND)),
                                ),
                            }
                        }
                        ControlPlaneAction::Resume
                            if record
                                .metadata
                                .get("unmaterialized_cook_admission")
                                .is_some_and(serde_json::Value::is_object) =>
                        {
                            if record.state.is_terminal() {
                                let terminal_error = record
                                    .metadata
                                    .get("cancel_reason")
                                    .and_then(serde_json::Value::as_str)
                                    .map(str::to_string)
                                    .or_else(|| {
                                        record.metadata["unmaterialized_cook_admission"]["reason"]
                                            .as_str()
                                            .map(str::to_string)
                                    });
                                (
                                    ControlPlaneActionOutcome::AlreadySatisfied,
                                    project_record(&record, None)?,
                                    ControlPlaneActionPayload {
                                        schema: "homeboy/unmaterialized-cook-resume/v1".to_string(),
                                        data: serde_json::json!({
                                            "schema": "homeboy/unmaterialized-cook-resume/v1",
                                            "status": record.metadata["unmaterialized_cook_admission"]["state"],
                                            "run_id": record.run_id,
                                            "idempotent": true,
                                            "terminal": true,
                                            "terminal_state": record.state,
                                            "error": terminal_error,
                                        }),
                                    },
                                    None,
                                )
                            } else {
                                let resumed = (|| -> homeboy_core::Result<_> {
                                    crate::agent_task_lifecycle::rearm_unmaterialized_cook_admission(
                                        requested_id.as_str(),
                                    )?;
                                    let reconciliation = crate::agent_task_service::reconcile_unmaterialized_cook_admission(
                                        requested_id.as_str(),
                                    )?;
                                    let current = self.lookup.store.read_record(&resolved)?;
                                    Ok((current, reconciliation))
                                })();
                                match resumed {
                                    Ok((current, reconciliation)) => (
                                        ControlPlaneActionOutcome::Succeeded,
                                        project_record(&current, None)?,
                                        ControlPlaneActionPayload {
                                            schema: "homeboy/unmaterialized-cook-resume/v1"
                                                .to_string(),
                                            data: serde_json::json!({
                                                "schema": "homeboy/unmaterialized-cook-resume/v1",
                                                "status": current.metadata["unmaterialized_cook_admission"]["state"],
                                                "run_id": requested_id,
                                                "idempotent": true,
                                                "reconciliation": reconciliation,
                                            }),
                                        },
                                        None,
                                    ),
                                    Err(error) => (
                                        ControlPlaneActionOutcome::Failed,
                                        project_record(&record, None)?,
                                        ControlPlaneActionPayload::empty(),
                                        Some(redacted_bounded(&error.message, MESSAGE_BOUND)),
                                    ),
                                }
                            }
                        }
                        ControlPlaneAction::Resume => {
                            let needs_transport_recovery =
                                crate::agent_task_service::terminal_transport_recovery_required(
                                    requested_id.as_str(),
                                );
                            let resumed = (|| -> homeboy_core::Result<_> {
                                if needs_transport_recovery {
                                    crate::agent_task_service::recover_terminal_transport_proxy_evidence(
                                        requested_id.as_str(),
                                    )?;
                                }
                                let result = resume()?;
                                if needs_transport_recovery {
                                    crate::agent_task_service::reconcile_terminal_artifact_projection(
                                        requested_id.as_str(),
                                    )?;
                                }
                                let current = self.lookup.store.read_record(&resolved)?;
                                Ok((result, current))
                            })();
                            match resumed {
                                Ok((result, current)) => (
                                    if record.state.is_terminal() {
                                        ControlPlaneActionOutcome::AlreadySatisfied
                                    } else {
                                        ControlPlaneActionOutcome::Succeeded
                                    },
                                    project_record(&current, None)?,
                                    ControlPlaneActionPayload {
                                        schema: CONTROL_PLANE_RESUME_RESULT_SCHEMA.to_string(),
                                        data: serde_json::json!({
                                            "aggregate": result.value,
                                            "exit_code": result.exit_code,
                                        }),
                                    },
                                    None,
                                ),
                                Err(error) => (
                                    ControlPlaneActionOutcome::Failed,
                                    project_record(&record, None)?,
                                    ControlPlaneActionPayload::empty(),
                                    Some(redacted_bounded(&error.message, MESSAGE_BOUND)),
                                ),
                            }
                        }
                    }
                };
                let result = ControlPlaneActionAcknowledgement {
                    schema: CONTROL_PLANE_ACTION_ACKNOWLEDGEMENT_SCHEMA.to_string(),
                    acknowledgement,
                    run: RunId::new(&record.run_id)
                        .map_err(|error| ControlPlaneError::invalid_argument(error.to_string()))?,
                    action: request.action,
                    idempotency_key: request.idempotency_key.clone(),
                    actor: request.actor.clone(),
                    accepted_at,
                    completed_at: now_timestamp(),
                    outcome,
                    resource,
                    result,
                    message,
                };
                complete_cook_operation_in_store(
                    &self.lookup.store,
                    &resolved,
                    &operation_key,
                    serde_json::to_value(&result).map_err(|error| {
                        ControlPlaneError::unavailable(format!("serialize action result: {error}"))
                    })?,
                )
                .map_err(map_lifecycle_error)?;
                Ok(result)
            }
        }
    }
}

fn review_cook_contract(
    store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<Option<(String, Value)>, ControlPlaneError> {
    let recipe_store = crate::agent_task_service::CookRecipeStore::new(store.roots().clone());
    let Some(recipe) = recipe_store
        .load_recipe_for_attempt(run_id)
        .map_err(map_lifecycle_error)?
    else {
        return Ok(None);
    };
    let base = recipe
        .finalization
        .get("base")
        .and_then(Value::as_str)
        .filter(|base| !base.trim().is_empty())
        .ok_or_else(|| {
            ControlPlaneError::invalid_argument(
                "durable Cook recipe is missing its declared promotion base",
            )
        })?
        .to_string();
    Ok(Some((base, recipe.gate_policy)))
}

fn review_failure_reasons(aggregate: &crate::agent_tasks::AgentTaskAggregate) -> Vec<Value> {
    let mut diagnostics = Vec::new();
    for outcome in aggregate.outcomes.iter().filter(|outcome| {
        matches!(
            outcome.status,
            crate::agent_tasks::AgentTaskOutcomeStatus::Failed
                | crate::agent_tasks::AgentTaskOutcomeStatus::ProviderError
                | crate::agent_tasks::AgentTaskOutcomeStatus::Timeout
                | crate::agent_tasks::AgentTaskOutcomeStatus::UnableToRemediate
                | crate::agent_tasks::AgentTaskOutcomeStatus::Cancelled
        )
    }) {
        for diagnostic in &outcome.diagnostics {
            diagnostics.push(serde_json::json!({
                "task_id": outcome.task_id,
                "class": redacted_bounded(&diagnostic.class, STATE_BOUND),
                "message": redacted_bounded(&diagnostic.message, MESSAGE_BOUND),
                "source": "diagnostics",
            }));
        }
        collect_nested_diagnostics(&outcome.outputs, &outcome.task_id, &mut diagnostics);
        collect_nested_diagnostics(&outcome.metadata, &outcome.task_id, &mut diagnostics);
    }
    diagnostics.sort_by_key(|diagnostic| {
        let class = diagnostic["class"].as_str().unwrap_or_default();
        let priority = if class.contains("validation") || class.contains("fatal") {
            0
        } else if class.contains("registration") || class.contains("missing") {
            1
        } else {
            2
        };
        (
            priority,
            class.to_string(),
            diagnostic["message"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        )
    });
    let mut seen = std::collections::BTreeSet::new();
    diagnostics
        .into_iter()
        .filter(|diagnostic| {
            seen.insert((
                diagnostic["class"].as_str().unwrap_or_default().to_string(),
                diagnostic["message"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            ))
        })
        .take(8)
        .collect()
}

fn collect_nested_diagnostics(value: &Value, task_id: &str, diagnostics: &mut Vec<Value>) {
    match value {
        Value::Object(object) => {
            if let Some(items) = object.get("diagnostics").and_then(Value::as_array) {
                for item in items {
                    if let (Some(class), Some(message)) = (
                        item.get("class").and_then(Value::as_str),
                        item.get("message").and_then(Value::as_str),
                    ) {
                        diagnostics.push(serde_json::json!({
                            "task_id": task_id,
                            "class": redacted_bounded(class, STATE_BOUND),
                            "message": redacted_bounded(message, MESSAGE_BOUND),
                            "source": "nested_diagnostics",
                        }));
                    }
                }
            }
            for nested in object.values() {
                collect_nested_diagnostics(nested, task_id, diagnostics);
            }
        }
        Value::Array(values) => {
            for nested in values {
                collect_nested_diagnostics(nested, task_id, diagnostics);
            }
        }
        _ => {}
    }
}

fn review_execution_states(
    aggregate: Option<&crate::agent_tasks::AgentTaskAggregate>,
    record: &AgentTaskRunRecord,
    resource: &ControlPlaneRun,
) -> Value {
    let review = aggregate.map(|aggregate| {
        crate::agent_tasks::AgentTaskAggregateReport::from(aggregate.outcomes.clone())
    });
    let canonical_candidate = review_canonical_candidate(record, review.as_ref(), resource);
    let candidate_state = canonical_candidate["state"]
        .as_str()
        .unwrap_or("not_available");
    let candidate_tasks = review
        .as_ref()
        .map(|review| {
            review
                .tasks
                .iter()
                .map(|task| {
                    let reason_code = if task.status
                        == crate::agent_tasks::AgentTaskOutcomeStatus::NoOp
                    {
                        "no_changes_produced"
                    } else {
                        match task.decision {
                            crate::agent_tasks::AgentTaskReconciliationDecision::NoOp => {
                                "no_changes_produced"
                            }
                            crate::agent_tasks::AgentTaskReconciliationDecision::ApplyCandidate => {
                                candidate_state
                            }
                            crate::agent_tasks::AgentTaskReconciliationDecision::RetryCandidate => {
                                "provider_retry_required"
                            }
                            crate::agent_tasks::AgentTaskReconciliationDecision::IssueReportCandidate => {
                                "issue_report_required"
                            }
                            crate::agent_tasks::AgentTaskReconciliationDecision::ReviewCandidate => {
                                "review_required"
                            }
                        }
                    };
                    serde_json::json!({ "task_id": task.task_id, "state": task.decision, "reason_code": reason_code })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let promotion = record.metadata.get("latest_promotion");
    let promotion_state = promotion
        .and_then(|promotion| promotion.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("not_attempted");
    let target_applied = promotion.is_some_and(review_promotion_target_applied);
    let accepted_inherited_failure = promotion
        .and_then(|promotion| promotion.get("deterministic_gates"))
        .and_then(Value::as_array)
        .is_some_and(|gates| {
            gates.iter().any(|gate| {
                gate.get("status").and_then(Value::as_str) == Some("accepted_inherited_failure")
            })
        });
    let finalization_state = record
        .metadata
        .pointer("/cook_finalization/status")
        .and_then(Value::as_str)
        .map(|status| match status {
            "review_ready" | "draft_published" => "completed",
            "failed" | "finalization_failed" => "finalization_failed",
            "pending" | "finalization_pending" => "finalization_pending",
            other => other,
        })
        .unwrap_or("not_attempted");
    serde_json::json!({
        "schema": "homeboy/agent-task-execution-states/v1",
        "provider": aggregate.map(|aggregate| aggregate.outcomes.iter().map(|outcome| {
            let failed = matches!(outcome.status, crate::agent_tasks::AgentTaskOutcomeStatus::Failed | crate::agent_tasks::AgentTaskOutcomeStatus::ProviderError | crate::agent_tasks::AgentTaskOutcomeStatus::Timeout | crate::agent_tasks::AgentTaskOutcomeStatus::UnableToRemediate | crate::agent_tasks::AgentTaskOutcomeStatus::Cancelled);
            serde_json::json!({ "task_id": outcome.task_id, "state": if failed { "failed" } else { "succeeded" }, "outcome_status": outcome.status, "failure_classification": outcome.failure_classification })
        }).collect::<Vec<_>>()).unwrap_or_default(),
        "candidate": { "state": candidate_state, "tasks": candidate_tasks },
        "gate": { "state": if accepted_inherited_failure { "accepted_inherited_failure" } else if !target_applied { "not_run" } else if matches!(promotion_state, "gate_failed" | "no_changes_gate_failed") { "failed" } else if promotion_state == "verification_pending" { "pending" } else if matches!(promotion_state, "applied" | "verified_no_changes") { "passed" } else { "not_run" } },
        "promotion": { "state": promotion_state, "patch_promoted": target_applied, "verified": target_applied && promotion_state == "applied", "verification_phase": if promotion_state == "verification_pending" && target_applied { "post_apply" } else if promotion_state == "verification_pending" { "pre_apply" } else { "not_pending" }, "target": { "state": if target_applied { "applied" } else if promotion.is_some() { "not_applied" } else { "not_declared" }, "worktree": promotion.and_then(|promotion| promotion.pointer("/target/worktree").or_else(|| promotion.get("to_worktree"))), "candidate_fingerprint_matches": target_applied && promotion.is_some_and(|promotion| promotion.pointer("/provenance/candidate").is_some_and(|candidate| !candidate.is_null())) } },
        "finalization": { "state": finalization_state, "finalized": finalization_state == "completed" },
        "publication": resource.publication,
    })
}

fn review_canonical_candidate(
    record: &AgentTaskRunRecord,
    review: Option<&crate::agent_tasks::AgentTaskAggregateReport>,
    resource: &ControlPlaneRun,
) -> Value {
    let promotion = record.metadata.get("latest_promotion");
    let promotion_status = promotion
        .and_then(|promotion| promotion.get("status"))
        .and_then(Value::as_str);
    let target_applied = promotion.is_some_and(review_promotion_target_applied);
    let finalized = record
        .metadata
        .get("cook_finalization")
        .is_some_and(|finalization| {
            matches!(
                finalization.get("status").and_then(Value::as_str),
                Some("review_ready" | "draft_published")
            ) && finalization
                .get("pr_url")
                .or_else(|| finalization.get("pull_request_url"))
                .and_then(Value::as_str)
                .is_some_and(|url| !url.trim().is_empty())
        });
    let retained = promotion.is_some_and(|promotion| {
        promotion_status.is_some_and(|status| {
            matches!(status, "applied" | "gate_failed" | "verification_pending")
        }) && promotion
            .get("patch_artifact")
            .or_else(|| promotion.get("patch"))
            .and_then(|artifact| artifact.get("id").or_else(|| artifact.get("artifact_id")))
            .and_then(Value::as_str)
            .is_some_and(|id| !id.trim().is_empty())
    });
    let state = if finalized {
        "finalized"
    } else if retained {
        "apply_ready"
    } else if review.is_some_and(|review| review.summary.apply_candidates > 0) {
        "patch_available"
    } else if review.is_some_and(|review| {
        !review.tasks.is_empty()
            && review
                .tasks
                .iter()
                .all(|task| task.status == crate::agent_tasks::AgentTaskOutcomeStatus::NoOp)
    }) {
        "no_changes_produced"
    } else {
        resource
            .candidate
            .as_ref()
            .map(|candidate| candidate.state.as_str())
            .unwrap_or("not_available")
    };
    serde_json::json!({
        "schema": "homeboy/agent-task-candidate/v1",
        "state": state,
        "id": resource.candidate.as_ref().and_then(|candidate| candidate.id.as_deref()),
        "target_applied": target_applied,
        "verified": target_applied && promotion_status == Some("applied"),
        "finalized": finalized,
        "fingerprint": promotion.and_then(|promotion| promotion.pointer("/provenance/candidate")),
    })
}

fn review_promotion_target_applied(promotion: &Value) -> bool {
    matches!(
        promotion.get("status").and_then(Value::as_str),
        Some("verification_pending" | "applied" | "gate_failed")
    ) && promotion
        .pointer("/provenance/post_apply")
        .and_then(Value::as_bool)
        == Some(true)
        && promotion
            .pointer("/patch_artifact/id")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.trim().is_empty())
        && promotion
            .pointer("/target/worktree")
            .or_else(|| promotion.get("to_worktree"))
            .and_then(Value::as_str)
            .is_some_and(|target| !target.trim().is_empty())
        && promotion
            .pointer("/provenance/candidate")
            .is_some_and(|candidate| !candidate.is_null())
}

fn review_next_actions(
    record: &AgentTaskRunRecord,
    review: Option<&crate::agent_tasks::AgentTaskAggregateReport>,
    has_target: bool,
) -> Vec<String> {
    if record.state == AgentTaskRunState::Queued {
        return vec!["run this queued durable task with `homeboy agent-task run <run-id>` or let a daemon claim it with `homeboy agent-task run-next`".to_string()];
    }
    if record.state == AgentTaskRunState::Running {
        return vec!["inspect progress with `homeboy agent-task status <run-id>` and `homeboy agent-task logs <run-id>`".to_string()];
    }
    let Some(review) = review else {
        return vec!["terminal run has no aggregate artifact; inspect lifecycle status for finalization errors".to_string()];
    };
    let mut actions = Vec::new();
    if review.summary.apply_candidates > 0 {
        actions.push(if has_target {
            "review `promotion_candidates` and run the generated promotion command".to_string()
        } else {
            format!(
                "rerun review with `homeboy agent-task review {} --to-worktree <managed-worktree>`",
                record.run_id
            )
        });
    }
    if review.summary.retry_candidates > 0 {
        actions.push(format!("retry provider-error or timeout candidates after fixing executor/preflight issues with `homeboy agent-task retry {} --run`", record.run_id));
        actions.push(format!("rerun the persisted plan through Lab with `homeboy --runner <runner-id> agent-task run-plan --plan @{} --record-run-id <new-run-id>`", record.plan_path));
    }
    if review.summary.issue_report_candidates > 0 {
        actions.push(
            "open or update the tracker with `issue_report_candidates` diagnostics and evidence"
                .to_string(),
        );
    }
    if review.summary.review_candidates > 0 {
        actions.push(
            "inspect `review_candidates` before deciding whether to retry, report, or ignore"
                .to_string(),
        );
    }
    if actions.is_empty() {
        actions.push("no promotion, retry, or issue-report candidates were produced; inspect task summaries for no-op completion".to_string());
    }
    actions
}

fn review_record_projection(record: &AgentTaskRunRecord) -> (Value, Vec<Value>) {
    let mut value = serde_json::to_value(record).unwrap_or(Value::Null);
    let Some(metadata) = value.get_mut("metadata").and_then(Value::as_object_mut) else {
        return (value, Vec::new());
    };
    let evidence = [
        "automatic_artifact_retention",
        "automatic_artifact_retention_inaccessible_roots",
    ]
    .into_iter()
    .filter_map(|key| {
        let details = metadata.remove(key)?;
        let count = details
            .get("worktree_count")
            .and_then(Value::as_u64)
            .or_else(|| details.as_array().map(|items| items.len() as u64))
            .or_else(|| details.get("worktrees").and_then(Value::as_array).map(|items| items.len() as u64))
            .unwrap_or(0);
        Some(serde_json::json!({
            "kind": key,
            "count": count,
            "details_omitted": true,
            "ref": format!("homeboy://agent-task/run/{}/status#metadata.{key}", record.run_id),
            "command": format!("homeboy agent-task status {}", record.run_id),
            "export_command": format!("homeboy agent-task status {} --output <path>", record.run_id),
        }))
    })
    .collect();
    (value, evidence)
}

fn bounded_review_evidence(value: Value) -> Value {
    let mut value = homeboy_core::redaction::redact_json(&value);
    redact_private_gate_programs(&mut value);
    let Some(fields) = value.as_object_mut() else {
        return value;
    };
    for field in fields.values_mut() {
        omit_oversized_review_field(field, REVIEW_EVIDENCE_FIELD_BOUND);
    }
    while json_size(&value) > REVIEW_EVIDENCE_BOUND {
        let largest = value.as_object().and_then(|fields| {
            fields
                .iter()
                .filter(|(_, field)| field.get("details_omitted").is_none())
                .max_by_key(|(_, field)| json_size(field))
                .map(|(key, _)| key.clone())
        });
        let Some(largest) = largest else {
            break;
        };
        let fields = value.as_object_mut().expect("review evidence object");
        let size = fields.get(&largest).map(json_size).unwrap_or_default();
        fields.insert(largest, omitted_review_field(size));
    }
    value
}

fn redact_private_gate_programs(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                if key == "private_verify" {
                    let count = value.as_array().map(Vec::len).unwrap_or(1);
                    *value = Value::Array(
                        std::iter::repeat_n(Value::String("[private]".to_string()), count)
                            .collect(),
                    );
                } else {
                    redact_private_gate_programs(value);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                redact_private_gate_programs(value);
            }
        }
        _ => {}
    }
}

fn omit_oversized_review_field(value: &mut Value, limit: usize) {
    let size = json_size(value);
    if size > limit {
        *value = omitted_review_field(size);
    }
}

fn omitted_review_field(size_bytes: usize) -> Value {
    serde_json::json!({
        "details_omitted": true,
        "reason": "review_evidence_byte_limit",
        "size_bytes": size_bytes,
    })
}

fn json_size(value: &Value) -> usize {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .unwrap_or(0)
}

impl OrchestrationService<LifecycleStoreLookup> {
    /// Reconcile an accepted cancellation through the canonical lifecycle owner.
    /// Runner probes stay disabled: cancellation convergence is controller-owned
    /// and a remote runner must not consume this bounded acknowledgement window.
    fn converge_cancellation(
        &self,
        accepted: &AgentTaskRunRecord,
    ) -> (AgentTaskRunRecord, ControlPlaneCancelResult) {
        let started = Instant::now();
        let mut polls = 0;
        let mut observed = accepted.clone();
        loop {
            match crate::agent_task_lifecycle::reconcile_status_in_store(
                &self.lookup.store,
                &accepted.run_id,
                crate::agent_task_lifecycle::AgentTaskStatusOptions {
                    runner_probe: crate::agent_task_lifecycle::AgentTaskRunnerProbe::Never,
                },
                false,
            ) {
                Ok(status) => {
                    observed = status.record;
                    if observed.state.is_terminal()
                        || observed
                            .metadata
                            .get("cancellation_deferred_for_terminal_provider")
                            .is_some()
                    {
                        return (
                            observed.clone(),
                            cancel_result_for_record(&observed, started.elapsed(), polls, None),
                        );
                    }
                }
                Err(error) => {
                    return (
                        observed.clone(),
                        cancel_result_for_record(
                            &observed,
                            started.elapsed(),
                            polls,
                            Some(redacted_bounded(&error.message, MESSAGE_BOUND)),
                        ),
                    );
                }
            }
            if started.elapsed() >= CANCEL_TERMINAL_WAIT {
                return (
                    observed.clone(),
                    cancel_result_for_record(&observed, started.elapsed(), polls, None),
                );
            }
            std::thread::sleep(CANCEL_TERMINAL_POLL_INTERVAL);
            polls += 1;
        }
    }
}

fn cancel_result_for_record(
    record: &AgentTaskRunRecord,
    waited: Duration,
    poll_count: u64,
    observation_error: Option<String>,
) -> ControlPlaneCancelResult {
    let disposition = if record.state == AgentTaskRunState::Cancelled {
        ControlPlaneCancelDisposition::Cancelled
    } else if record.state.is_terminal() {
        ControlPlaneCancelDisposition::TerminalWithoutCancellation
    } else if record
        .metadata
        .get("cancellation_deferred_for_terminal_provider")
        .is_some()
    {
        ControlPlaneCancelDisposition::DeferredForTerminalProvider
    } else {
        ControlPlaneCancelDisposition::Requested
    };
    ControlPlaneCancelResult {
        schema: CONTROL_PLANE_CANCEL_RESULT_SCHEMA.to_string(),
        disposition,
        terminal: record.state.is_terminal(),
        wait_timeout_seconds: CANCEL_TERMINAL_WAIT.as_secs(),
        waited_seconds: waited.as_secs(),
        poll_count,
        observation_error,
    }
}

fn cancel_result_payload(result: ControlPlaneCancelResult) -> ControlPlaneActionPayload {
    ControlPlaneActionPayload {
        schema: CONTROL_PLANE_CANCEL_RESULT_SCHEMA.to_string(),
        data: serde_json::to_value(result).expect("control-plane cancellation result serializes"),
    }
}

fn promotion_handoff(report: &crate::agent_task_promotion::AgentTaskPromotionReport) -> Value {
    let target_applied = report.status.patch_promoted();
    let verified = matches!(
        report.status,
        crate::agent_task_promotion::AgentTaskPromotionStatus::Applied
    );
    let next_action = if report.status.gate_failed() {
        "patch promoted but deterministic gates failed; use gate feedback before finalizing"
    } else if target_applied && verified {
        "patch promoted and deterministic gates verified; finalize a PR"
    } else if target_applied {
        "patch promoted into the target worktree; verify, then finalize a PR"
    } else {
        "dry run only; rerun promote without `--dry-run` before finalizing"
    };

    serde_json::json!({
        "schema": "homeboy/agent-task-promotion-handoff/v1",
        "states": {
            "patch_artifact_produced": true,
            "candidate_retained": true,
            "target_applied": target_applied,
            "patch_promoted": target_applied,
            "verified": verified,
            "finalized": false,
            "pr_opened": false,
        },
        "boundary": report.status.handoff_boundary(),
        "finalize_command": report.source.run_id.as_ref().map(|run_id| format!(
            "homeboy agent-task finalize-pr --recover {run_id}"
        )),
        "next_actions": [next_action],
    })
}

fn validate_action_request(request: &ControlPlaneActionRequest) -> Result<(), ControlPlaneError> {
    if request.schema != CONTROL_PLANE_ACTION_REQUEST_SCHEMA {
        return Err(ControlPlaneError::invalid_argument(
            "unsupported control-plane action request schema",
        ));
    }
    for (name, value) in [
        ("idempotency_key", request.idempotency_key.as_str()),
        ("actor", request.actor.as_str()),
    ] {
        if value.trim().is_empty() || value.len() > ACTION_INPUT_BOUND {
            return Err(ControlPlaneError::invalid_argument(format!(
                "{name} must contain 1 to {ACTION_INPUT_BOUND} bytes"
            )));
        }
    }
    let expected_parameters_schema = match request.action {
        ControlPlaneAction::Cancel => CONTROL_PLANE_CANCEL_PARAMETERS_SCHEMA,
        ControlPlaneAction::Promote => CONTROL_PLANE_PROMOTE_PARAMETERS_SCHEMA,
        ControlPlaneAction::Reconcile => CONTROL_PLANE_EMPTY_ACTION_PAYLOAD_SCHEMA,
        ControlPlaneAction::Resume => CONTROL_PLANE_EMPTY_ACTION_PAYLOAD_SCHEMA,
        ControlPlaneAction::Retry => CONTROL_PLANE_RETRY_PARAMETERS_SCHEMA,
    };
    if request.parameters.schema != expected_parameters_schema {
        return Err(ControlPlaneError::invalid_argument(format!(
            "{} requires parameters schema {expected_parameters_schema}",
            action_name(request.action)
        )));
    }
    if request.action == ControlPlaneAction::Cancel {
        let parameters: ControlPlaneCancelParameters =
            serde_json::from_value(request.parameters.data.clone()).map_err(|error| {
                ControlPlaneError::invalid_argument(format!("cancel parameters: {error}"))
            })?;
        if parameters
            .reason
            .as_ref()
            .is_some_and(|reason| reason.len() > ACTION_REASON_BOUND)
        {
            return Err(ControlPlaneError::invalid_argument(format!(
                "reason exceeds {ACTION_REASON_BOUND} bytes"
            )));
        }
    }
    if request.action == ControlPlaneAction::Retry {
        serde_json::from_value::<ControlPlaneRetryParameters>(request.parameters.data.clone())
            .map_err(|error| {
                ControlPlaneError::invalid_argument(format!("retry parameters: {error}"))
            })?;
    }
    if request.action == ControlPlaneAction::Promote {
        serde_json::from_value::<crate::agent_task_service::AgentTaskPromotionRequest>(
            request.parameters.data.clone(),
        )
        .map_err(|error| {
            ControlPlaneError::invalid_argument(format!("promote parameters: {error}"))
        })?;
    }
    if matches!(
        request.action,
        ControlPlaneAction::Cancel | ControlPlaneAction::Promote | ControlPlaneAction::Retry
    ) && !request.confirmed
    {
        return Err(ControlPlaneError::invalid_argument(format!(
            "{} requires explicit confirmation",
            action_name(request.action)
        )));
    }
    Ok(())
}

const fn action_name(action: ControlPlaneAction) -> &'static str {
    match action {
        ControlPlaneAction::Cancel => "cancel",
        ControlPlaneAction::Resume => "resume",
        ControlPlaneAction::Retry => "retry",
        ControlPlaneAction::Promote => "promote",
        ControlPlaneAction::Reconcile => "reconcile",
    }
}

fn map_lifecycle_error(error: homeboy_core::Error) -> ControlPlaneError {
    if error.code == homeboy_core::ErrorCode::ValidationInvalidArgument {
        ControlPlaneError::invalid_argument(error.message)
    } else {
        ControlPlaneError::unavailable(error.message)
    }
}

impl<L: EventLookup> OrchestrationService<L> {
    pub fn events(
        &self,
        requested_id: &RunId,
        cursor: Option<&homeboy_control_plane_contract::EventCursor>,
    ) -> Result<homeboy_control_plane_contract::ControlPlaneEventPage, ControlPlaneError> {
        if cursor.is_some_and(|cursor| cursor.as_str().parse::<u64>().is_err()) {
            return Err(ControlPlaneError::invalid_argument(
                "control-plane event cursor is invalid",
            ));
        }
        self.lookup.events(requested_id, cursor)?.ok_or_else(|| {
            ControlPlaneError::not_found(format!("agent-task run not found: {requested_id}"))
        })
    }
}

/// Read one canonical run resource from the current controller installation.
pub fn run_from_current_environment(run_id: &str) -> homeboy_core::Result<ControlPlaneRun> {
    let requested_id = RunId::new(run_id).map_err(|error| {
        homeboy_core::Error::validation_invalid_argument(
            "run_id",
            error.to_string(),
            Some(run_id.to_string()),
            None,
        )
    })?;
    let store = AgentTaskLifecycleStore::from_current_environment()?;
    OrchestrationService::new(LifecycleStoreLookup::new(store))
        .run(&requested_id)
        .map_err(|error| match error.class {
            ControlPlaneErrorClass::NotFound | ControlPlaneErrorClass::InvalidArgument => {
                homeboy_core::Error::validation_invalid_argument(
                    "run_id",
                    error.message,
                    Some(run_id.to_string()),
                    None,
                )
            }
            ControlPlaneErrorClass::Unavailable => {
                homeboy_core::Error::internal_unexpected(error.message)
            }
        })
}

pub fn review_from_current_environment(
    run_id: &str,
    request: &ControlPlaneRunReviewRequest,
) -> homeboy_core::Result<ControlPlaneRunReview> {
    let requested_id = RunId::new(run_id).map_err(|error| {
        homeboy_core::Error::validation_invalid_argument(
            "run_id",
            error.to_string(),
            Some(run_id.to_string()),
            None,
        )
    })?;
    let store = AgentTaskLifecycleStore::from_current_environment()?;
    OrchestrationService::new(LifecycleStoreLookup::new(store))
        .review(&requested_id, request)
        .map_err(|error| match error.class {
            ControlPlaneErrorClass::NotFound | ControlPlaneErrorClass::InvalidArgument => {
                homeboy_core::Error::validation_invalid_argument(
                    "run_id",
                    error.message,
                    Some(run_id.to_string()),
                    None,
                )
            }
            ControlPlaneErrorClass::Unavailable => {
                homeboy_core::Error::internal_unexpected(error.message)
            }
        })
}

pub fn execute_action_from_current_environment(
    run_id: &str,
    request: &ControlPlaneActionRequest,
) -> homeboy_core::Result<ControlPlaneActionAcknowledgement> {
    execute_action_from_current_environment_with_delegates(
        run_id,
        request,
        |parameters| default_retry(run_id, parameters),
        || default_resume(run_id),
        default_promote,
    )
}

pub fn execute_retry_action_from_current_environment_with_preflight<F>(
    run_id: &str,
    request: &ControlPlaneActionRequest,
    preflight: F,
) -> homeboy_core::Result<ControlPlaneActionAcknowledgement>
where
    F: Fn(&AgentTaskPlan) -> homeboy_core::Result<()>,
{
    execute_action_from_current_environment_with_delegates(
        run_id,
        request,
        |parameters| {
            crate::agent_task_service::retry_with_preflight(
                run_id,
                parameters.new_run_id.as_deref(),
                true,
                parameters.force,
                &preflight,
            )
        },
        || default_resume(run_id),
        default_promote,
    )
}

pub fn execute_resume_action_from_current_environment(
    run_id: &str,
    request: &ControlPlaneActionRequest,
    executor: crate::agent_task_scheduler::SharedAgentTaskExecutor,
) -> homeboy_core::Result<ControlPlaneActionAcknowledgement> {
    execute_action_from_current_environment_with_delegates(
        run_id,
        request,
        |parameters| default_retry(run_id, parameters),
        || crate::agent_task_service::resume(run_id.to_string(), executor),
        default_promote,
    )
}

pub fn execute_promotion_action_from_current_environment(
    run_id: &str,
    request: &ControlPlaneActionRequest,
    progress: Option<crate::agent_task_promotion::PromotionProgressCallback>,
) -> homeboy_core::Result<ControlPlaneActionAcknowledgement> {
    execute_action_from_current_environment_with_delegates(
        run_id,
        request,
        |parameters| default_retry(run_id, parameters),
        || default_resume(run_id),
        |promotion| {
            crate::agent_task_service::execute_promotion_with_progress(promotion.clone(), progress)
        },
    )
}

fn execute_action_from_current_environment_with_delegates<F, R, P>(
    run_id: &str,
    request: &ControlPlaneActionRequest,
    retry: F,
    resume: R,
    promote: P,
) -> homeboy_core::Result<ControlPlaneActionAcknowledgement>
where
    F: FnOnce(
        &ControlPlaneRetryParameters,
    ) -> homeboy_core::Result<crate::agent_task_service::AgentTaskRetryServiceResult>,
    R: FnOnce() -> homeboy_core::Result<
        crate::agent_task_service::AgentTaskRunResult<
            crate::agent_task_schedule::AgentTaskAggregate,
        >,
    >,
    P: FnOnce(
        &crate::agent_task_service::AgentTaskPromotionRequest,
    ) -> homeboy_core::Result<crate::agent_task_promotion::AgentTaskPromotionReport>,
{
    let requested_id = RunId::new(run_id).map_err(|error| {
        homeboy_core::Error::validation_invalid_argument(
            "run_id",
            error.to_string(),
            Some(run_id.to_string()),
            None,
        )
    })?;
    let store = AgentTaskLifecycleStore::from_current_environment()?;
    OrchestrationService::new(LifecycleStoreLookup::new(store))
        .execute_action_with_delegates(&requested_id, request, retry, resume, promote)
        .map_err(|error| match error.class {
            ControlPlaneErrorClass::NotFound | ControlPlaneErrorClass::InvalidArgument => {
                homeboy_core::Error::validation_invalid_argument(
                    "action",
                    error.message,
                    None,
                    None,
                )
            }
            ControlPlaneErrorClass::Unavailable => {
                homeboy_core::Error::internal_unexpected(error.message)
            }
        })
}

fn default_retry(
    run_id: &str,
    parameters: &ControlPlaneRetryParameters,
) -> homeboy_core::Result<crate::agent_task_service::AgentTaskRetryServiceResult> {
    crate::agent_task_service::retry(
        run_id,
        parameters.new_run_id.as_deref(),
        true,
        parameters.force,
    )
}

fn default_resume(
    run_id: &str,
) -> homeboy_core::Result<
    crate::agent_task_service::AgentTaskRunResult<crate::agent_task_schedule::AgentTaskAggregate>,
> {
    crate::agent_task_service::resume(
        run_id.to_string(),
        std::sync::Arc::new(
            crate::agent_task_provider::ExtensionProviderAgentTaskExecutor::discover(),
        ),
    )
}

fn default_promote(
    promotion: &crate::agent_task_service::AgentTaskPromotionRequest,
) -> homeboy_core::Result<crate::agent_task_promotion::AgentTaskPromotionReport> {
    crate::agent_task_service::execute_promotion(promotion.clone())
}

/// Project a durable record and optional plan the caller already loaded.
pub fn project_record(
    record: &AgentTaskRunRecord,
    plan: Option<&AgentTaskPlan>,
) -> Result<ControlPlaneRun, ControlPlaneError> {
    let run = RunId::new(&record.run_id)
        .map_err(|error| ControlPlaneError::invalid_argument(format!("durable run id: {error}")))?;
    let identities = identities_for_record(record)?;
    let mut resource = ControlPlaneRun::new(run);
    if let Some(identities) = identities {
        resource.mission = Some(identities.mission);
        resource.attempt = Some(identities.attempt);
        resource.attempt_number = Some(identities.attempt_number);
    }
    resource.state = run_state(record);
    resource.location = location(record);
    resource.execution = execution(record)?;
    resource.phase = phase(record);
    resource.blocker = blocker(record);
    resource.owner = Some(owner(record));
    resource.runtime = runtime(record);
    resource.provider = assigned_provider(record);
    resource.heartbeat_at = heartbeat_at(record);
    resource.created_at = record.submitted_at.clone();
    resource.updated_at = record.updated_at.clone();
    resource.liveness = live_provider_liveness(record, plan, Utc::now());
    if let Some(observed_at) = resource
        .liveness
        .as_ref()
        .and_then(|liveness| liveness.last_observed_progress_at.as_deref())
    {
        resource.updated_at = newest_timestamp(resource.updated_at.as_deref(), Some(observed_at));
    }
    resource.candidate = candidate(record);
    resource.gates = gates(record);
    resource.publication = publication(record);
    resource.action_eligibility = Some(lifecycle_action_eligibility(record, plan));
    if resource.state.is_terminal() {
        resource.finished_at = record.updated_at.clone();
    }
    resource.evidence = evidence_refs(record);
    resource.artifacts = artifact_refs(record);
    Ok(resource)
}

/// Apply one opaque resume cursor and a fixed page bound to an ordered stream.
pub fn event_page(
    run: RunId,
    events: Vec<homeboy_control_plane_contract::ControlPlaneEvent>,
    cursor: Option<&homeboy_control_plane_contract::EventCursor>,
) -> Result<homeboy_control_plane_contract::ControlPlaneEventPage, ControlPlaneError> {
    use homeboy_control_plane_contract::{
        ControlPlaneEventPage, EventCursor, CONTROL_PLANE_EVENT_PAGE_SCHEMA,
    };

    let after = cursor
        .map(|cursor| {
            cursor.as_str().parse::<u64>().map_err(|_| {
                ControlPlaneError::invalid_argument("control-plane event cursor is invalid")
            })
        })
        .transpose()?
        .unwrap_or(0);
    let mut remaining = events.into_iter().filter(|event| event.sequence > after);
    let page_events: Vec<_> = remaining.by_ref().take(EVENT_PAGE_BOUND).collect();
    let has_more = remaining.next().is_some();
    let next_cursor = page_events
        .last()
        .map(|event| event.sequence.to_string())
        .or_else(|| cursor.map(|cursor| cursor.as_str().to_string()))
        .map(EventCursor::new)
        .transpose()
        .map_err(|error| ControlPlaneError::invalid_argument(error.to_string()))?;

    Ok(ControlPlaneEventPage {
        schema: CONTROL_PLANE_EVENT_PAGE_SCHEMA.to_string(),
        run,
        events: page_events,
        next_cursor,
        has_more,
    })
}

fn identities_for_record(
    record: &AgentTaskRunRecord,
) -> Result<Option<CanonicalControlPlaneIdentities>, ControlPlaneError> {
    canonical_control_plane_identities(record)
        .map_err(|error| ControlPlaneError::invalid_argument(error.message))
}

fn run_state(record: &AgentTaskRunRecord) -> ControlPlaneRunState {
    if record.is_stale_running() {
        return ControlPlaneRunState::Stale;
    }
    match record.state {
        AgentTaskRunState::Queued => ControlPlaneRunState::Queued,
        AgentTaskRunState::Running => ControlPlaneRunState::Running,
        AgentTaskRunState::Succeeded => ControlPlaneRunState::Succeeded,
        AgentTaskRunState::CandidateRecoverable => ControlPlaneRunState::CandidateRecoverable,
        AgentTaskRunState::PartialRecoverable => ControlPlaneRunState::PartialRecoverable,
        AgentTaskRunState::PartialFailure => ControlPlaneRunState::PartialFailure,
        AgentTaskRunState::Failed => ControlPlaneRunState::Failed,
        AgentTaskRunState::Cancelled => ControlPlaneRunState::Cancelled,
    }
}

fn location(record: &AgentTaskRunRecord) -> Option<ControlPlaneLocation> {
    let runner_id = record.runner_id().map(str::to_string);
    let transport = record
        .metadata
        .get("remote_run_id")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    if runner_id.is_none() && transport.is_none() {
        return None;
    }
    Some(ControlPlaneLocation {
        runner_id,
        remote_run_id: transport,
    })
}

fn execution(record: &AgentTaskRunRecord) -> Result<Option<ExecutionId>, ControlPlaneError> {
    let Some(job_id) = record.runner_job_id() else {
        return Ok(None);
    };
    ExecutionId::new(job_id)
        .map(Some)
        .map_err(|error| ControlPlaneError::invalid_argument(error.to_string()))
}

fn phase(record: &AgentTaskRunRecord) -> Option<String> {
    if has_running_provider_execution(record) {
        return Some("provider_execution".to_string());
    }
    record
        .metadata
        .pointer("/cook_progress/phase")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .map(|value| bounded(value, STATE_BOUND))
        .or_else(|| {
            record
                .candidate_adoption
                .as_ref()
                .map(|adoption| bounded(&adoption.phase, STATE_BOUND))
                .filter(|value| !value.is_empty())
        })
}

fn has_running_provider_execution(record: &AgentTaskRunRecord) -> bool {
    record.state == AgentTaskRunState::Running
        && record.metadata["provider_executions"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|execution| execution["state"].as_str() == Some("running"))
}

/// Read metadata for the finite stream references recorded at provider launch.
/// No provider content is opened and no artifact directory is enumerated.
fn live_provider_liveness(
    record: &AgentTaskRunRecord,
    plan: Option<&AgentTaskPlan>,
    now: DateTime<Utc>,
) -> Option<ControlPlaneLiveness> {
    if record.state != AgentTaskRunState::Running {
        return None;
    }
    let executions = record.metadata["provider_executions"].as_array()?;
    let execution = executions
        .iter()
        .rev()
        .find(|execution| execution["state"].as_str() == Some("running"))?;
    let window_seconds = execution
        .get("task_id")
        .and_then(Value::as_str)
        .and_then(|task_id| {
            plan.and_then(|plan| {
                plan.tasks
                    .iter()
                    .find(|task| task.task_id == task_id)
                    .and_then(|task| task.limits.liveness_timeout_ms)
            })
        })
        .map(|milliseconds| milliseconds.saturating_add(999) / 1_000)
        .unwrap_or_else(|| {
            crate::agent_task_timeout::effective_provider_liveness_timeout_ms(None)
                .saturating_add(999)
                / 1_000
        });
    let mut observations = Vec::new();
    for (source, fields) in [
        ("structured_progress", ["structured_progress"].as_slice()),
        ("runtime_output", ["stdout", "stderr"].as_slice()),
    ] {
        for field in fields {
            if let Some(observed_at) = execution
                .pointer(&format!("/runtime_evidence/{field}"))
                .and_then(Value::as_str)
                .and_then(observed_file_timestamp)
            {
                observations.push((source, observed_at));
            }
        }
    }
    if let Some(observed_at) = execution
        .get("workspace_activity_observed_at")
        .and_then(Value::as_str)
        .and_then(parse_timestamp)
    {
        observations.push(("workspace_activity", observed_at));
    }
    let started_at = execution
        .get("started_at")
        .and_then(Value::as_str)
        .and_then(parse_timestamp);
    let latest = observations
        .into_iter()
        .max_by_key(|(_, observed_at)| *observed_at);
    let (source, observed_at) = match latest {
        Some((source, observed_at)) => (Some(source.to_string()), Some(observed_at)),
        None => (None, started_at),
    };
    let age_seconds = observed_at
        .map(|observed_at| now.signed_duration_since(observed_at).num_seconds().max(0) as u64)
        .unwrap_or(u64::MAX);
    Some(ControlPlaneLiveness {
        state: if age_seconds <= window_seconds {
            "active"
        } else {
            "silent"
        }
        .to_string(),
        source,
        last_observed_progress_at: observed_at.map(|observed_at| observed_at.to_rfc3339()),
        age_seconds,
        window_seconds,
    })
}

fn observed_file_timestamp(uri: &str) -> Option<DateTime<Utc>> {
    let path = uri.strip_prefix("file://")?;
    let metadata = std::fs::metadata(Path::new(path)).ok()?;
    let modified = metadata.modified().ok()?;
    Some(DateTime::<Utc>::from(modified))
}

fn parse_timestamp(timestamp: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

fn newest_timestamp(existing: Option<&str>, observed: Option<&str>) -> Option<String> {
    match (
        existing.and_then(parse_timestamp),
        observed.and_then(parse_timestamp),
    ) {
        (Some(existing), Some(observed)) => Some(existing.max(observed).to_rfc3339()),
        (Some(existing), None) => Some(existing.to_rfc3339()),
        (None, Some(observed)) => Some(observed.to_rfc3339()),
        (None, None) => None,
    }
}

fn blocker(record: &AgentTaskRunRecord) -> Option<ControlPlaneBlocker> {
    if let Some(quarantine) = record.metadata.get("queue_quarantine") {
        let message = quarantine
            .get("reason")
            .and_then(|value| value.as_str())
            .unwrap_or("run is quarantined");
        return Some(ControlPlaneBlocker {
            code: Some("quarantine".to_string()),
            message: redacted_bounded(message, MESSAGE_BOUND),
        });
    }
    if let Some(reason) = record.stale_running_reason() {
        return Some(ControlPlaneBlocker {
            code: Some("stale".to_string()),
            message: redacted_bounded(reason, MESSAGE_BOUND),
        });
    }
    if let Some(state) = record
        .metadata
        .pointer("/unmaterialized_cook_admission/state")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
    {
        return Some(ControlPlaneBlocker {
            code: Some("unmaterialized".to_string()),
            message: redacted_bounded(state, MESSAGE_BOUND),
        });
    }
    if let Some(message) = record
        .metadata
        .pointer("/cook_controller_failure/message")
        .or_else(|| record.metadata.pointer("/cook_controller_failure/detail"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
    {
        return Some(ControlPlaneBlocker {
            code: Some("controller_failure".to_string()),
            message: redacted_bounded(message, MESSAGE_BOUND),
        });
    }
    record
        .candidate_adoption
        .as_ref()
        .and_then(|adoption| adoption.terminal_error.as_deref())
        .filter(|value| !value.trim().is_empty())
        .map(|message| ControlPlaneBlocker {
            code: Some("adoption".to_string()),
            message: redacted_bounded(message, MESSAGE_BOUND),
        })
}

fn owner(record: &AgentTaskRunRecord) -> ControlPlaneOwner {
    match record.runner_id() {
        Some(runner_id) => ControlPlaneOwner {
            kind: "runner".to_string(),
            id: bounded(runner_id, ID_BOUND),
        },
        None => ControlPlaneOwner {
            kind: "local_controller".to_string(),
            id: "local_controller".to_string(),
        },
    }
}

fn runtime(record: &AgentTaskRunRecord) -> Option<ControlPlaneRuntime> {
    record
        .metadata
        .pointer("/controller_runtime/originating/build_identity")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .map(|value| ControlPlaneRuntime {
            build_identity: redacted_bounded(value, MESSAGE_BOUND),
        })
}

fn assigned_provider(record: &AgentTaskRunRecord) -> Option<ControlPlaneProviderSummary> {
    if let Some(evidence) = record.latest_executor_evidence.as_ref() {
        return Some(ControlPlaneProviderSummary {
            id: bounded(&evidence.backend, ID_BOUND),
            state: None,
            session: evidence
                .provider_run_id
                .as_deref()
                .and_then(|id| ProviderSessionId::new(id).ok()),
        });
    }
    if let Some(handle) = record.provider_handles.first() {
        return Some(ControlPlaneProviderSummary {
            id: bounded(&handle.backend, ID_BOUND),
            state: handle.state.as_ref().map(|state| {
                bounded(
                    &serde_json::to_value(state)
                        .ok()
                        .and_then(|value| value.as_str().map(str::to_string))
                        .unwrap_or_else(|| "unknown".to_string()),
                    STATE_BOUND,
                )
            }),
            session: ProviderSessionId::new(&handle.provider_run_id).ok(),
        });
    }
    record
        .metadata
        .get("provider_executions")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .find(|execution| {
            execution.get("state").and_then(|value| value.as_str()) == Some("running")
        })
        .and_then(|execution| {
            let id = execution.get("backend").and_then(|value| value.as_str())?;
            Some(ControlPlaneProviderSummary {
                id: bounded(id, ID_BOUND),
                state: Some("running".to_string()),
                session: execution
                    .get("provider_run_id")
                    .and_then(|value| value.as_str())
                    .and_then(|id| ProviderSessionId::new(id).ok()),
            })
        })
}

fn heartbeat_at(record: &AgentTaskRunRecord) -> Option<String> {
    record
        .lifecycle
        .heartbeat
        .as_ref()
        .map(|heartbeat| heartbeat.last_seen_at.clone())
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            record
                .candidate_adoption
                .as_ref()
                .map(|adoption| adoption.heartbeat_at.clone())
                .filter(|value| !value.trim().is_empty())
        })
}

fn candidate(record: &AgentTaskRunRecord) -> Option<ControlPlaneStateSummary> {
    if let Some(adoption) = record.candidate_adoption.as_ref() {
        return Some(ControlPlaneStateSummary {
            id: nonempty_bounded(&adoption.candidate_sha, ID_BOUND),
            state: bounded(&adoption.state, STATE_BOUND),
        });
    }
    let promoted = record
        .metadata
        .pointer("/latest_promotion/status")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .map(|state| ControlPlaneStateSummary {
            id: record
                .metadata
                .pointer("/latest_promotion/task_id")
                .and_then(|value| value.as_str())
                .and_then(|value| nonempty_bounded(value, ID_BOUND)),
            state: bounded(state, STATE_BOUND),
        });
    if promoted.is_some() {
        return promoted;
    }
    if matches!(
        record.state,
        AgentTaskRunState::CandidateRecoverable | AgentTaskRunState::PartialRecoverable
    ) {
        return record
            .artifact_refs
            .iter()
            .find(|artifact| artifact.kind == "patch")
            .map(|artifact| ControlPlaneStateSummary {
                id: nonempty_redacted_bounded(
                    artifact.label.as_deref().unwrap_or(&artifact.task_id),
                    ID_BOUND,
                ),
                state: "patch_available".to_string(),
            });
    }
    None
}

fn gates(record: &AgentTaskRunRecord) -> Vec<ControlPlaneStateSummary> {
    record
        .metadata
        .get("latest_promotion")
        .and_then(|promotion| {
            promotion
                .get("deterministic_gates")
                .or_else(|| promotion.get("gate_results"))
        })
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|gate| {
            let state = gate
                .get("status")
                .or_else(|| gate.get("state"))
                .and_then(|value| value.as_str())
                .filter(|value| !value.trim().is_empty())?;
            let id = gate
                .get("id")
                .or_else(|| gate.get("kind"))
                .or_else(|| gate.get("type"))
                .and_then(|value| value.as_str())
                .and_then(|value| nonempty_bounded(value, ID_BOUND));
            Some(ControlPlaneStateSummary {
                id,
                state: bounded(state, STATE_BOUND),
            })
        })
        .take(GATE_BOUND)
        .collect()
}

fn publication(record: &AgentTaskRunRecord) -> Option<ControlPlaneStateSummary> {
    let finalization = record.metadata.get("cook_finalization")?;
    let state = finalization
        .get("status")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())?;
    let id = ["pr_number", "pr_url", "pull_request_url"]
        .into_iter()
        .find_map(|key| finalization.get(key))
        .and_then(|value| {
            value
                .as_str()
                .map(str::to_string)
                .or_else(|| value.as_u64().map(|number| number.to_string()))
        })
        .and_then(|value| nonempty_redacted_bounded(&value, MESSAGE_BOUND));
    Some(ControlPlaneStateSummary {
        id,
        state: bounded(state, STATE_BOUND),
    })
}

fn evidence_refs(record: &AgentTaskRunRecord) -> Vec<ControlPlaneEvidenceRef> {
    record
        .latest_executor_evidence
        .iter()
        .flat_map(|evidence| evidence.refs())
        .enumerate()
        .map(|(index, evidence)| ControlPlaneEvidenceRef {
            id: redacted_bounded(
                &evidence
                    .label
                    .unwrap_or_else(|| format!("evidence-{}", index + 1)),
                ID_BOUND,
            ),
            kind: redacted_bounded(&evidence.kind, STATE_BOUND),
            uri: redacted_bounded(&evidence.uri, URI_BOUND),
        })
        .take(REF_BOUND)
        .collect()
}

fn artifact_refs(record: &AgentTaskRunRecord) -> Vec<ControlPlaneEvidenceRef> {
    record
        .artifact_refs
        .iter()
        .map(|artifact| ControlPlaneEvidenceRef {
            id: redacted_bounded(
                &artifact
                    .label
                    .clone()
                    .unwrap_or_else(|| artifact.task_id.clone()),
                ID_BOUND,
            ),
            kind: redacted_bounded(&artifact.kind, STATE_BOUND),
            uri: redacted_bounded(&artifact.uri, URI_BOUND),
        })
        .take(REF_BOUND)
        .collect()
}

fn bounded(value: &str, max: usize) -> String {
    let mut chars = value.chars();
    let truncated: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        truncated
    } else {
        value.to_string()
    }
}

fn nonempty_bounded(value: &str, max: usize) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| bounded(trimmed, max))
}

pub(crate) fn redacted_bounded(value: &str, max: usize) -> String {
    bounded(&homeboy_core::redaction::redact_string(value), max)
}

fn nonempty_redacted_bounded(value: &str, max: usize) -> Option<String> {
    nonempty_bounded(&homeboy_core::redaction::redact_string(value), max)
}

fn is_run_not_found(error: &homeboy_core::Error) -> bool {
    error.code == homeboy_core::ErrorCode::ValidationInvalidArgument
        && error.message.contains("not found")
}

struct RegisteredProvider;

impl ControlPlaneProvider for RegisteredProvider {
    fn capabilities(&self) -> ControlPlaneCapabilities {
        OrchestrationService::<LifecycleStoreLookup>::capabilities()
    }

    fn run(&self, requested_id: &RunId) -> Result<ControlPlaneRun, ControlPlaneError> {
        let store = AgentTaskLifecycleStore::from_environment()
            .map_err(|error| ControlPlaneError::unavailable(error.message))?;
        OrchestrationService::new(LifecycleStoreLookup::new(store)).run(requested_id)
    }

    fn review(
        &self,
        requested_id: &RunId,
        request: &ControlPlaneRunReviewRequest,
    ) -> Result<ControlPlaneRunReview, ControlPlaneError> {
        let store = AgentTaskLifecycleStore::from_environment()
            .map_err(|error| ControlPlaneError::unavailable(error.message))?;
        OrchestrationService::new(LifecycleStoreLookup::new(store)).review(requested_id, request)
    }

    fn events(
        &self,
        requested_id: &RunId,
        cursor: Option<&homeboy_control_plane_contract::EventCursor>,
    ) -> Result<homeboy_control_plane_contract::ControlPlaneEventPage, ControlPlaneError> {
        let store = AgentTaskLifecycleStore::from_environment()
            .map_err(|error| ControlPlaneError::unavailable(error.message))?;
        OrchestrationService::new(LifecycleStoreLookup::new(store)).events(requested_id, cursor)
    }

    fn execute_action(
        &self,
        requested_id: &RunId,
        request: &ControlPlaneActionRequest,
    ) -> Result<ControlPlaneActionAcknowledgement, ControlPlaneError> {
        let store = AgentTaskLifecycleStore::from_environment()
            .map_err(|error| ControlPlaneError::unavailable(error.message))?;
        OrchestrationService::new(LifecycleStoreLookup::new(store))
            .execute_action(requested_id, request)
    }
}

/// Register the orchestration service as the HTTP control-plane provider.
pub fn register() {
    register_control_plane_provider(Box::new(RegisteredProvider));
}

#[cfg(test)]
mod tests {
    use super::{
        bounded_review_evidence, event_page, live_provider_liveness, observed_file_timestamp,
        phase, project_record, review_failure_reasons, LifecycleStoreLookup, OrchestrationService,
        RunLookup, RunSnapshot, REVIEW_EVIDENCE_BOUND,
    };
    use crate::agent_task_lifecycle::{
        AgentTaskLifecycleStore, AgentTaskRunRecord, AgentTaskRunState,
    };
    use crate::agent_task_schedule::AgentTaskPlan;
    use homeboy_control_plane_contract::{
        ControlPlaneAction, ControlPlaneActionAvailability, ControlPlaneActionOutcome,
        ControlPlaneActionPayload, ControlPlaneActionRequest, ControlPlaneCancelDisposition,
        ControlPlaneCancelResult, ControlPlaneErrorClass, ControlPlaneEvent,
        ControlPlaneEventSource, ControlPlaneOperation, ControlPlaneRunReviewRequest,
        ControlPlaneRunState, EventCursor, EventId, RunId, CONTROL_PLANE_ACTION_ELIGIBILITY_SCHEMA,
        CONTROL_PLANE_ACTION_REQUEST_SCHEMA, CONTROL_PLANE_CANCEL_PARAMETERS_SCHEMA,
        CONTROL_PLANE_EVENT_SCHEMA, CONTROL_PLANE_PROMOTE_PARAMETERS_SCHEMA,
        CONTROL_PLANE_PROMOTE_RESULT_SCHEMA, CONTROL_PLANE_RESUME_RESULT_SCHEMA,
        CONTROL_PLANE_RUN_SCHEMA,
    };
    use homeboy_core::run_lifecycle_record::RunHeartbeat;
    use homeboy_core::test_support::with_isolated_home;
    use serde_json::json;
    use std::collections::BTreeMap;

    const AGENT_TASK_COOK: &str = "agent-task-301a2b9a-a63d-446b-a918-e21b2ff6421e";
    const AGENT_TASK_RUN: &str =
        "agent-task-301a2b9a-a63d-446b-a918-e21b2ff6421e-attempt-1-ea6a6751";

    struct MapLookup {
        snapshots: BTreeMap<String, RunSnapshot>,
    }

    impl RunLookup for MapLookup {
        fn get(
            &self,
            id: &RunId,
        ) -> Result<Option<RunSnapshot>, homeboy_control_plane_contract::ControlPlaneError>
        {
            Ok(self.snapshots.get(id.as_str()).cloned())
        }
    }

    fn record(run_id: &str) -> AgentTaskRunRecord {
        let mut record: AgentTaskRunRecord = serde_json::from_value(json!({
            "schema": "homeboy/agent-task-run/v1",
            "run_id": run_id,
            "plan_id": "plan",
            "state": "succeeded",
            "submitted_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:01:00Z",
            "plan_path": "/plan",
            "artifact_refs": [{
                "task_id": "review",
                "kind": "review_form",
                "uri": "homeboy://artifact/review"
            }],
            "provider_handles": [{
                "task_id": "review",
                "backend": "claude",
                "provider_run_id": "sess-1"
            }],
            "metadata": {
                "cook_attempt": 1,
                "runner_id": "homeboy-lab",
                "runner_job_id": "job-1",
                "remote_run_id": "remote-1",
                "cook_progress": { "phase": "terminal" },
                "controller_runtime": {
                    "originating": { "build_identity": "homeboy 0.1.0+test" }
                },
                "latest_promotion": {
                    "status": "applied",
                    "task_id": "review",
                    "deterministic_gates": [{ "id": "test", "status": "passed" }]
                },
                "cook_finalization": {
                    "status": "published",
                    "pr_url": "https://example.invalid/pr/1"
                },
                "cook_controller_failure": {
                    "message": "provider failed token=super-secret"
                }
            }
        }))
        .expect("record");
        record.plan_path = "/secret/workspace".to_string();
        record.lifecycle.heartbeat = Some(RunHeartbeat {
            last_seen_at: "2026-01-01T00:00:30Z".to_string(),
            owner_pid: None,
            stale_after_seconds: None,
        });
        record
    }

    fn event(run: &RunId, sequence: u64) -> ControlPlaneEvent {
        ControlPlaneEvent {
            schema: CONTROL_PLANE_EVENT_SCHEMA.to_string(),
            event: EventId::new(format!("{}:event:{sequence}", run.as_str())).expect("event"),
            sequence,
            occurred_at: None,
            mission: None,
            run: run.clone(),
            task: None,
            attempt: None,
            execution: None,
            kind: "run.progress".to_string(),
            source: ControlPlaneEventSource {
                component: "test".to_string(),
                instance: None,
            },
            data: json!({ "sequence": sequence }),
            artifacts: Vec::new(),
            evidence: Vec::new(),
        }
    }

    #[test]
    fn live_provider_liveness_projects_newer_structured_progress_without_reading_content() {
        let temp = tempfile::tempdir().expect("tempdir");
        let runtime_output = temp.path().join("provider-runtime-stdout.log");
        let structured_progress = temp.path().join("provider-progress.jsonl");
        std::fs::write(&runtime_output, "runtime output").expect("runtime output");
        std::fs::write(&structured_progress, "provider-secret-content").expect("progress");
        let runtime_output_at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000);
        let structured_progress_at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_005);
        std::fs::File::open(&runtime_output)
            .expect("open runtime output")
            .set_times(std::fs::FileTimes::new().set_modified(runtime_output_at))
            .expect("set runtime output timestamp");
        std::fs::File::open(&structured_progress)
            .expect("open structured progress")
            .set_times(std::fs::FileTimes::new().set_modified(structured_progress_at))
            .expect("set structured progress timestamp");
        let observed_at =
            observed_file_timestamp(&format!("file://{}", structured_progress.display()))
                .expect("structured progress timestamp");
        let mut record = record(AGENT_TASK_RUN);
        record.state = AgentTaskRunState::Running;
        record.updated_at = Some("1970-01-01T00:00:01Z".to_string());
        record.metadata["provider_executions"] = json!([{
            "task_id": "review",
            "state": "running",
            "started_at": "2020-01-01T00:00:00Z",
            "runtime_evidence": {
                "stdout": format!("file://{}", runtime_output.display()),
                "structured_progress": format!("file://{}", structured_progress.display()),
            },
        }]);

        let liveness =
            live_provider_liveness(&record, None, observed_at + chrono::Duration::seconds(5))
                .expect("running provider liveness");

        assert_eq!(liveness.state, "active");
        assert_eq!(liveness.source.as_deref(), Some("structured_progress"));
        assert_eq!(
            liveness.last_observed_progress_at.as_deref(),
            Some(observed_at.to_rfc3339().as_str())
        );
        assert_eq!(liveness.age_seconds, 5);
        assert_eq!(phase(&record).as_deref(), Some("provider_execution"));
        assert!(!serde_json::to_string(&liveness)
            .expect("liveness JSON")
            .contains("provider-secret-content"));

        let resource = project_record(&record, None).expect("project running record");
        assert_eq!(
            resource.updated_at.as_deref(),
            Some(observed_at.to_rfc3339().as_str())
        );
    }

    #[test]
    fn silent_provider_becomes_silent_after_its_liveness_window() {
        let mut record = record(AGENT_TASK_RUN);
        record.state = AgentTaskRunState::Running;
        record.metadata["provider_executions"] = json!([{
            "task_id": "review",
            "state": "running",
            "started_at": "2020-01-01T00:00:00Z",
        }]);

        let liveness = live_provider_liveness(
            &record,
            None,
            chrono::DateTime::parse_from_rfc3339("2020-01-02T00:00:00Z")
                .expect("timestamp")
                .with_timezone(&chrono::Utc),
        )
        .expect("running provider liveness");

        assert_eq!(liveness.state, "silent");
        assert_eq!(liveness.source, None);
        assert_eq!(
            liveness.last_observed_progress_at.as_deref(),
            Some("2020-01-01T00:00:00+00:00")
        );
        assert!(liveness.age_seconds > liveness.window_seconds);
    }

    #[test]
    fn live_provider_liveness_uses_durable_workspace_activity() {
        let mut record = record(AGENT_TASK_RUN);
        record.state = AgentTaskRunState::Running;
        record.metadata["provider_executions"] = json!([{
            "task_id": "review",
            "state": "running",
            "started_at": "2020-01-01T00:00:00Z",
            "workspace_activity_observed_at": "2020-01-01T00:00:05Z",
        }]);

        let liveness = live_provider_liveness(
            &record,
            None,
            chrono::DateTime::parse_from_rfc3339("2020-01-01T00:00:10Z")
                .expect("timestamp")
                .with_timezone(&chrono::Utc),
        )
        .expect("running provider liveness");

        assert_eq!(liveness.state, "active");
        assert_eq!(liveness.source.as_deref(), Some("workspace_activity"));
        assert_eq!(
            liveness.last_observed_progress_at.as_deref(),
            Some("2020-01-01T00:00:05+00:00")
        );
    }

    #[test]
    fn event_pages_are_bounded_and_resume_after_the_opaque_cursor() {
        let run = RunId::new("run-events").expect("run");
        let events = (1..=101).map(|sequence| event(&run, sequence)).collect();
        let first = event_page(run.clone(), events, None).expect("first page");
        assert_eq!(first.events.len(), 100);
        assert!(first.has_more);
        assert_eq!(
            first.next_cursor.as_ref().map(EventCursor::as_str),
            Some("100")
        );

        let second = event_page(
            run,
            vec![event(&RunId::new("run-events").unwrap(), 101)],
            first.next_cursor.as_ref(),
        )
        .expect("second page");
        assert_eq!(second.events.len(), 1);
        assert_eq!(second.events[0].sequence, 101);
        assert!(!second.has_more);
    }

    #[test]
    fn event_pages_reject_unknown_cursor_encodings() {
        let run = RunId::new("run-events").expect("run");
        let cursor = EventCursor::new("not-a-provider-cursor").expect("typed opaque cursor");
        let error = event_page(run, Vec::new(), Some(&cursor)).expect_err("invalid cursor");
        assert_eq!(error.class, ControlPlaneErrorClass::InvalidArgument);
    }

    fn snapshot(run_id: &str, plan: Option<AgentTaskPlan>) -> RunSnapshot {
        RunSnapshot {
            record: record(run_id),
            plan,
        }
    }

    fn service() -> OrchestrationService<MapLookup> {
        let mut snapshots = BTreeMap::new();
        snapshots.insert(AGENT_TASK_RUN.to_string(), snapshot(AGENT_TASK_RUN, None));
        OrchestrationService::new(MapLookup { snapshots })
    }

    fn eligibility(
        resource: &homeboy_control_plane_contract::ControlPlaneRun,
        action: ControlPlaneAction,
    ) -> ControlPlaneActionAvailability {
        resource
            .action_eligibility
            .as_ref()
            .expect("projected action eligibility")
            .actions
            .iter()
            .find(|candidate| candidate.action == action)
            .expect("action")
            .availability
    }

    #[test]
    fn capabilities_advertise_only_wired_operations() {
        let capabilities = OrchestrationService::<LifecycleStoreLookup>::capabilities();
        assert_eq!(
            capabilities.operations,
            vec![
                ControlPlaneOperation::GetCapabilities,
                ControlPlaneOperation::GetRun,
                ControlPlaneOperation::GetRunEvents,
                ControlPlaneOperation::GetRunReview,
                ControlPlaneOperation::ExecuteRunAction,
            ]
        );
        assert!(!capabilities.operations.is_empty());
    }

    #[test]
    fn review_preserves_authoritative_aggregate_absence() {
        with_isolated_home(|_| {
            let store = AgentTaskLifecycleStore::from_current_environment().expect("store");
            store.write_record(&record(AGENT_TASK_RUN)).expect("record");
            let service = OrchestrationService::new(LifecycleStoreLookup::new(store));

            let review = service
                .review(
                    &RunId::new(AGENT_TASK_RUN).expect("run"),
                    &ControlPlaneRunReviewRequest::default(),
                )
                .expect("review");

            assert!(review.evidence["aggregate"].is_null());
            assert_eq!(
                review.evidence["read"]["unavailable_sources"][0]["source"],
                "aggregate"
            );
            assert_eq!(
                review.evidence["read"]["unavailable_sources"][0]["reason_code"],
                "durable_read.authoritative_aggregate_absent"
            );
        });
    }

    #[test]
    fn review_rejects_conflicting_provider_inputs() {
        with_isolated_home(|_| {
            let store = AgentTaskLifecycleStore::from_current_environment().expect("store");
            let service = OrchestrationService::new(LifecycleStoreLookup::new(store));
            let error = service
                .review(
                    &RunId::new("missing-run").expect("run"),
                    &ControlPlaneRunReviewRequest {
                        to_worktree: None,
                        provider_command: Some("provider".to_string()),
                        provider_argv: vec!["provider".to_string()],
                    },
                )
                .expect_err("conflicting provider inputs");

            assert_eq!(error.class, ControlPlaneErrorClass::InvalidArgument);
        });
    }

    #[test]
    fn review_evidence_is_redacted_and_byte_bounded() {
        let evidence = bounded_review_evidence(json!({
            "record": { "api_token": "super-secret" },
            "aggregate": "x".repeat(REVIEW_EVIDENCE_BOUND * 2),
        }));
        let serialized = serde_json::to_vec(&evidence).expect("evidence JSON");

        assert!(!String::from_utf8_lossy(&serialized).contains("super-secret"));
        assert!(serialized.len() <= REVIEW_EVIDENCE_BOUND);
        assert_eq!(evidence["aggregate"]["details_omitted"], true);
    }

    #[test]
    fn review_failure_diagnostics_are_redacted_bounded_and_failure_only() {
        let mut aggregate: crate::agent_tasks::AgentTaskAggregate = serde_json::from_value(json!({
            "schema": "homeboy/agent-task-aggregate/v1",
            "plan_id": "plan",
            "status": "failed",
            "totals": { "skipped": 0 }
        }))
        .expect("aggregate");
        aggregate.outcomes = vec![
            crate::agent_tasks::AgentTaskOutcome {
                task_id: "failed".to_string(),
                status: crate::agent_tasks::AgentTaskOutcomeStatus::Failed,
                outputs: json!({ "diagnostics": [{
                    "class": "provider.error",
                    "message": format!("token=secret {}", "x".repeat(1_000))
                }] }),
                ..Default::default()
            },
            crate::agent_tasks::AgentTaskOutcome {
                task_id: "success".to_string(),
                status: crate::agent_tasks::AgentTaskOutcomeStatus::Succeeded,
                diagnostics: vec![serde_json::from_value(json!({
                    "class": "provider.success",
                    "message": "successful wrapper diagnostic"
                }))
                .expect("diagnostic")],
                ..Default::default()
            },
        ];

        let reasons = review_failure_reasons(&aggregate);

        assert_eq!(reasons.len(), 1);
        assert!(
            reasons[0]["message"].as_str().is_some_and(|message| message
                .starts_with("token=[REDACTED]")
                && message.len() <= 259)
        );
    }

    #[test]
    fn review_redacts_automatic_retention_inventory() {
        with_isolated_home(|_| {
            let store = AgentTaskLifecycleStore::from_current_environment().expect("store");
            let mut run = record(AGENT_TASK_RUN);
            run.metadata["automatic_artifact_retention"] = json!({
                "worktree_count": 2,
                "worktrees": ["/private/unrelated-one", "/private/unrelated-two"],
            });
            run.metadata["automatic_artifact_retention_inaccessible_roots"] = json!({
                "worktrees": ["/private/inaccessible"],
            });
            store.write_record(&run).expect("record");
            let service = OrchestrationService::new(LifecycleStoreLookup::new(store));

            let review = service
                .review(
                    &RunId::new(AGENT_TASK_RUN).expect("run"),
                    &ControlPlaneRunReviewRequest::default(),
                )
                .expect("review");

            assert!(review.evidence["record"]["metadata"]
                .get("automatic_artifact_retention")
                .is_none());
            assert!(review.evidence["record"]["metadata"]
                .get("automatic_artifact_retention_inaccessible_roots")
                .is_none());
            assert_eq!(review.evidence["cleanup_evidence"][0]["count"], 2);
            assert_eq!(review.evidence["cleanup_evidence"][1]["count"], 1);
            let serialized = serde_json::to_string(&review).expect("review JSON");
            assert!(!serialized.contains("super-secret"));
            assert!(!serialized.contains("/private/unrelated-one"));
            assert!(!serialized.contains("/private/inaccessible"));
        });
    }

    #[test]
    fn terminal_cancel_is_replayed_and_conflicting_key_reuse_is_rejected() {
        with_isolated_home(|_| {
            let store = AgentTaskLifecycleStore::from_current_environment().expect("store");
            store.write_record(&record(AGENT_TASK_RUN)).expect("record");
            let service = OrchestrationService::new(LifecycleStoreLookup::new(store));
            let run = RunId::new(AGENT_TASK_RUN).expect("run");
            let request = ControlPlaneActionRequest {
                schema: CONTROL_PLANE_ACTION_REQUEST_SCHEMA.to_string(),
                action: ControlPlaneAction::Cancel,
                idempotency_key: "cancel-request-1".to_string(),
                actor: "test".to_string(),
                expected_updated_at: Some("2026-01-01T00:01:00Z".to_string()),
                parameters: ControlPlaneActionPayload {
                    schema: CONTROL_PLANE_CANCEL_PARAMETERS_SCHEMA.to_string(),
                    data: json!({ "reason": "no longer needed" }),
                },
                confirmed: true,
            };
            let first = service
                .execute_action(&run, &request)
                .expect("first action");
            assert_eq!(first.outcome, ControlPlaneActionOutcome::AlreadySatisfied);
            let result: ControlPlaneCancelResult =
                serde_json::from_value(first.result.data.clone())
                    .expect("typed cancellation result");
            assert_eq!(
                result.disposition,
                ControlPlaneCancelDisposition::TerminalWithoutCancellation
            );
            assert!(result.terminal);
            assert_eq!(
                service.execute_action(&run, &request).expect("replay"),
                first
            );
            let events = service.events(&run, None).expect("action events");
            let action_kinds: Vec<_> = events
                .events
                .iter()
                .filter(|event| event.kind.starts_with("action."))
                .map(|event| event.kind.as_str())
                .collect();
            assert_eq!(
                action_kinds,
                vec!["action.accepted", "action.already_satisfied"]
            );

            let mut conflicting = request;
            conflicting.parameters.data = json!({ "reason": "different reason" });
            let error = service
                .execute_action(&run, &conflicting)
                .expect_err("conflicting key");
            assert_eq!(error.class, ControlPlaneErrorClass::InvalidArgument);

            let stale = ControlPlaneActionRequest {
                idempotency_key: "cancel-request-stale".to_string(),
                expected_updated_at: Some("2025-12-31T23:59:59Z".to_string()),
                ..conflicting
            };
            let acknowledgement = service
                .execute_action(&run, &stale)
                .expect("failed acknowledgement");
            assert_eq!(acknowledgement.outcome, ControlPlaneActionOutcome::Failed);
            assert!(acknowledgement
                .message
                .as_deref()
                .is_some_and(|message| message.contains("precondition")));

            let reconcile = ControlPlaneActionRequest {
                schema: CONTROL_PLANE_ACTION_REQUEST_SCHEMA.to_string(),
                action: ControlPlaneAction::Reconcile,
                idempotency_key: "reconcile-request-1".to_string(),
                actor: "test".to_string(),
                expected_updated_at: None,
                parameters: ControlPlaneActionPayload::empty(),
                confirmed: true,
            };
            let first = service
                .execute_action(&run, &reconcile)
                .expect("reconcile action");
            assert_eq!(first.outcome, ControlPlaneActionOutcome::AlreadySatisfied);
            assert_eq!(first.result.schema, "homeboy/agent-task-reconcile/v1");
            assert_eq!(
                service.execute_action(&run, &reconcile).expect("replay"),
                first
            );
        });
    }

    #[test]
    fn accepted_cancel_returns_the_converged_resource_and_replays_without_waiting_again() {
        with_isolated_home(|_| {
            let store = AgentTaskLifecycleStore::from_current_environment().expect("store");
            crate::agent_task_lifecycle::submit_plan_in_store(
                &store,
                &AgentTaskPlan::new("cancel-converges", Vec::new()),
                Some(AGENT_TASK_RUN),
            )
            .expect("queued record");
            let service = OrchestrationService::new(LifecycleStoreLookup::new(store));
            let run = RunId::new(AGENT_TASK_RUN).expect("run");
            let request = ControlPlaneActionRequest {
                schema: CONTROL_PLANE_ACTION_REQUEST_SCHEMA.to_string(),
                action: ControlPlaneAction::Cancel,
                idempotency_key: "cancel-converges-1".to_string(),
                actor: "test".to_string(),
                expected_updated_at: None,
                parameters: ControlPlaneActionPayload {
                    schema: CONTROL_PLANE_CANCEL_PARAMETERS_SCHEMA.to_string(),
                    data: json!({ "reason": "not selected" }),
                },
                confirmed: true,
            };

            let first = service.execute_action(&run, &request).expect("cancel");
            let result: ControlPlaneCancelResult =
                serde_json::from_value(first.result.data.clone())
                    .expect("typed cancellation result");
            assert_eq!(first.outcome, ControlPlaneActionOutcome::Succeeded);
            assert_eq!(first.resource.state, ControlPlaneRunState::Cancelled);
            assert_eq!(result.disposition, ControlPlaneCancelDisposition::Cancelled);
            assert!(result.terminal);
            assert_eq!(result.poll_count, 0);
            assert_eq!(
                service.execute_action(&run, &request).expect("replay"),
                first
            );
        });
    }

    #[test]
    fn reconcile_action_mutates_and_completes_in_its_explicit_store() {
        with_isolated_home(|home| {
            let store =
                AgentTaskLifecycleStore::from_data_root(home.path().join("explicit-control-plane"));
            crate::agent_task_lifecycle::submit_plan_in_store(
                &store,
                &AgentTaskPlan::new("explicit-reconcile", Vec::new()),
                Some(AGENT_TASK_RUN),
            )
            .expect("explicit queued record");
            store
                .mutate_record(AGENT_TASK_RUN, |record| {
                    record.submitted_at = "2000-01-01T00:00:00Z".to_string();
                    record.updated_at = None;
                    true
                })
                .expect("stale explicit record");

            let service = OrchestrationService::new(LifecycleStoreLookup::new(store.clone()));
            let run = RunId::new(AGENT_TASK_RUN).expect("run");
            let request = ControlPlaneActionRequest {
                schema: CONTROL_PLANE_ACTION_REQUEST_SCHEMA.to_string(),
                action: ControlPlaneAction::Reconcile,
                idempotency_key: "explicit-reconcile-1".to_string(),
                actor: "test".to_string(),
                expected_updated_at: None,
                parameters: ControlPlaneActionPayload::empty(),
                confirmed: true,
            };

            let first = service
                .execute_action(&run, &request)
                .expect("reconcile action");
            assert_eq!(
                first.outcome,
                ControlPlaneActionOutcome::Succeeded,
                "{first:?}"
            );
            assert_eq!(
                store
                    .read_record(AGENT_TASK_RUN)
                    .expect("reconciled record")
                    .state,
                AgentTaskRunState::Cancelled
            );
            assert_eq!(
                service.execute_action(&run, &request).expect("replay"),
                first
            );
        });
    }

    #[test]
    fn resume_action_replays_the_stored_result_without_reexecuting() {
        with_isolated_home(|_| {
            let store = AgentTaskLifecycleStore::from_current_environment().expect("store");
            let mut terminal = record(AGENT_TASK_RUN);
            terminal.state = AgentTaskRunState::Running;
            terminal.metadata = json!({});
            terminal.artifact_refs.clear();
            terminal.provider_handles.clear();
            store.write_record(&terminal).expect("record");
            let aggregate = serde_json::from_value(json!({
                "schema": "homeboy/agent-task-aggregate/v1",
                "plan_id": "plan",
                "status": "succeeded",
                "totals": { "skipped": 0, "succeeded": 0, "failed": 0 },
                "outcomes": [],
            }))
            .expect("aggregate");
            store
                .write_aggregate(AGENT_TASK_RUN, &aggregate)
                .expect("aggregate evidence");
            let service = OrchestrationService::new(LifecycleStoreLookup::new(store));
            let run = RunId::new(AGENT_TASK_RUN).expect("run");
            let request = ControlPlaneActionRequest {
                schema: CONTROL_PLANE_ACTION_REQUEST_SCHEMA.to_string(),
                action: ControlPlaneAction::Resume,
                idempotency_key: "resume-request-1".to_string(),
                actor: "test".to_string(),
                expected_updated_at: None,
                parameters: ControlPlaneActionPayload::empty(),
                confirmed: true,
            };
            let executions = std::rc::Rc::new(std::cell::Cell::new(0));
            let execute = || {
                let executions = std::rc::Rc::clone(&executions);
                let aggregate = aggregate.clone();
                service.execute_action_with_delegates(
                    &run,
                    &request,
                    |_| panic!("retry delegate must not run"),
                    move || {
                        executions.set(executions.get() + 1);
                        Ok(crate::agent_task_service::AgentTaskRunResult {
                            value: aggregate,
                            exit_code: 0,
                        })
                    },
                    |_| panic!("promote delegate must not run"),
                )
            };

            let first = execute().expect("resume");
            let replay = execute().expect("replay");

            assert_eq!(
                first.outcome,
                ControlPlaneActionOutcome::Succeeded,
                "{first:?}"
            );
            assert_eq!(first.result.schema, CONTROL_PLANE_RESUME_RESULT_SCHEMA);
            assert_eq!(replay, first);
            assert_eq!(executions.get(), 1);
        });
    }

    #[test]
    fn failed_promotion_action_replays_without_reexecuting() {
        with_isolated_home(|_| {
            let store = AgentTaskLifecycleStore::from_current_environment().expect("store");
            store.write_record(&record(AGENT_TASK_RUN)).expect("record");
            let service = OrchestrationService::new(LifecycleStoreLookup::new(store));
            let run = RunId::new(AGENT_TASK_RUN).expect("run");
            let request = ControlPlaneActionRequest {
                schema: CONTROL_PLANE_ACTION_REQUEST_SCHEMA.to_string(),
                action: ControlPlaneAction::Promote,
                idempotency_key: "promote-request-1".to_string(),
                actor: "test".to_string(),
                expected_updated_at: None,
                parameters: ControlPlaneActionPayload {
                    schema: CONTROL_PLANE_PROMOTE_PARAMETERS_SCHEMA.to_string(),
                    data: json!({
                        "source": "{}",
                        "source_run_id": AGENT_TASK_RUN,
                        "to_worktree": "repo@candidate",
                        "dry_run": true,
                    }),
                },
                confirmed: true,
            };
            let mut malformed = request.clone();
            malformed.parameters.data = json!({ "source": "{}" });
            let error = service
                .execute_action_with_delegates(
                    &run,
                    &malformed,
                    |_| panic!("retry delegate must not run"),
                    || panic!("resume delegate must not run"),
                    |_| panic!("promote delegate must not run"),
                )
                .expect_err("malformed parameters");
            assert_eq!(error.class, ControlPlaneErrorClass::InvalidArgument);
            let executions = std::rc::Rc::new(std::cell::Cell::new(0));
            let execute = || {
                let executions = std::rc::Rc::clone(&executions);
                service.execute_action_with_delegates(
                    &run,
                    &request,
                    |_| panic!("retry delegate must not run"),
                    || panic!("resume delegate must not run"),
                    move |_| {
                        executions.set(executions.get() + 1);
                        Err(homeboy_core::Error::internal_unexpected(
                            "promotion fixture failed",
                        ))
                    },
                )
            };

            let first = execute().expect("failed acknowledgement");
            let replay = execute().expect("failed replay");

            assert_eq!(first.outcome, ControlPlaneActionOutcome::Failed);
            assert_eq!(replay, first);
            assert_eq!(executions.get(), 1);
        });
    }

    #[test]
    fn promotion_action_owns_handoff_recording_and_idempotent_replay() {
        with_isolated_home(|_| {
            let store = AgentTaskLifecycleStore::from_current_environment().expect("store");
            store.write_record(&record(AGENT_TASK_RUN)).expect("record");
            let service = OrchestrationService::new(LifecycleStoreLookup::new(store));
            let run = RunId::new(AGENT_TASK_RUN).expect("run");
            let request = ControlPlaneActionRequest {
                schema: CONTROL_PLANE_ACTION_REQUEST_SCHEMA.to_string(),
                action: ControlPlaneAction::Promote,
                idempotency_key: "promote-request-1".to_string(),
                actor: "test".to_string(),
                expected_updated_at: None,
                parameters: ControlPlaneActionPayload {
                    schema: CONTROL_PLANE_PROMOTE_PARAMETERS_SCHEMA.to_string(),
                    data: json!({
                        "source": "{}",
                        "source_run_id": AGENT_TASK_RUN,
                        "to_worktree": "homeboy@candidate",
                        "dry_run": false,
                    }),
                },
                confirmed: true,
            };
            let report: crate::agent_task_promotion::AgentTaskPromotionReport =
                serde_json::from_value(json!({
                "schema": "homeboy/agent-task-promotion-report/v1",
                "status": "applied",
                "source": { "kind": "aggregate", "run_id": AGENT_TASK_RUN, "task_id": "task" },
                "to_worktree": "homeboy@candidate",
                "target": { "worktree": "homeboy@candidate" },
                "patch_artifact": { "id": "patch-1", "kind": "patch", "path": "patch" },
                "operator_notification": { "status": "completed", "message": "complete" },
                }))
                .expect("promotion report");
            let executions = std::rc::Rc::new(std::cell::Cell::new(0));
            let execute = || {
                let executions = std::rc::Rc::clone(&executions);
                let report = report.clone();
                service.execute_action_with_delegates(
                    &run,
                    &request,
                    |_| panic!("retry delegate must not run"),
                    || panic!("resume delegate must not run"),
                    move |_| {
                        executions.set(executions.get() + 1);
                        Ok(report)
                    },
                )
            };

            let first = execute().expect("promote");
            let replay = execute().expect("replay");

            assert_eq!(first.result.schema, CONTROL_PLANE_PROMOTE_RESULT_SCHEMA);
            assert_eq!(
                first.result.data["handoff"]["states"]["target_applied"],
                true
            );
            assert_eq!(
                first.result.data["recorded_on_run"]["metadata_key"],
                "latest_promotion"
            );
            assert_eq!(replay, first);
            assert_eq!(executions.get(), 1);
        });
    }

    #[test]
    fn run_projects_canonical_detail_and_redacts_durable_payload() {
        let resource = service()
            .run(&RunId::new(AGENT_TASK_RUN).expect("run id"))
            .expect("run");
        assert_eq!(resource.schema, CONTROL_PLANE_RUN_SCHEMA);
        assert_eq!(resource.run.as_str(), AGENT_TASK_RUN);
        assert_eq!(
            resource.mission.as_ref().map(|id| id.as_str()),
            Some(AGENT_TASK_COOK)
        );
        assert_eq!(resource.attempt_number, Some(1));
        assert_eq!(resource.state, ControlPlaneRunState::Succeeded);
        assert_eq!(resource.phase.as_deref(), Some("terminal"));
        assert_eq!(
            resource
                .blocker
                .as_ref()
                .map(|blocker| blocker.message.as_str()),
            Some("provider failed token=[REDACTED]")
        );
        assert_eq!(
            resource.owner.as_ref().map(|owner| owner.kind.as_str()),
            Some("runner")
        );
        assert_eq!(
            resource
                .runtime
                .as_ref()
                .map(|runtime| runtime.build_identity.as_str()),
            Some("homeboy 0.1.0+test")
        );
        assert_eq!(
            resource
                .provider
                .as_ref()
                .map(|provider| provider.id.as_str()),
            Some("claude")
        );
        assert_eq!(
            resource.heartbeat_at.as_deref(),
            Some("2026-01-01T00:00:30Z")
        );
        assert_eq!(
            resource
                .candidate
                .as_ref()
                .map(|candidate| candidate.state.as_str()),
            Some("applied")
        );
        assert_eq!(resource.gates.len(), 1);
        assert_eq!(
            resource
                .publication
                .as_ref()
                .map(|publication| publication.state.as_str()),
            Some("published")
        );
        assert_eq!(
            resource
                .action_eligibility
                .as_ref()
                .expect("action eligibility")
                .schema,
            CONTROL_PLANE_ACTION_ELIGIBILITY_SCHEMA
        );
        assert_eq!(
            resource.execution.as_ref().map(|id| id.as_str()),
            Some("job-1")
        );
        assert_eq!(
            resource
                .location
                .as_ref()
                .and_then(|location| location.runner_id.as_deref()),
            Some("homeboy-lab")
        );
        assert_eq!(resource.artifacts.len(), 1);
        let value = serde_json::to_value(&resource).expect("serialize");
        assert!(value.get("metadata").is_none());
        assert!(value.get("cwd").is_none());
        assert!(value.get("plan_path").is_none());
        assert!(value.get("prompt").is_none());
        let decoded: homeboy_control_plane_contract::ControlPlaneRun =
            serde_json::from_value(value).expect("deserialize");
        assert_eq!(decoded, resource);
    }

    #[test]
    fn gate_failed_promotion_projects_a_recoverable_candidate_not_success() {
        let mut record = record(AGENT_TASK_RUN);
        record.state = crate::agent_task_lifecycle::AgentTaskRunState::CandidateRecoverable;
        record.lifecycle.execution.state =
            homeboy_core::run_lifecycle_record::RunExecutionState::CandidateRecoverable;
        record.metadata["latest_promotion"]["status"] = json!("gate_failed");
        record.metadata["latest_promotion"]["deterministic_gates"] = json!([
            { "id": "test", "status": "failed" }
        ]);
        record.metadata["cook_finalization"] = serde_json::Value::Null;

        let resource = project_record(&record, None).expect("project gate failure");

        assert_eq!(resource.state, ControlPlaneRunState::CandidateRecoverable);
        assert_eq!(
            resource
                .candidate
                .as_ref()
                .map(|candidate| candidate.state.as_str()),
            Some("gate_failed")
        );
        assert_eq!(resource.gates[0].state, "failed");
        assert!(resource.publication.is_none());
    }

    #[test]
    fn injected_plan_is_used_once_for_retry_eligibility() {
        let mut failed = record(AGENT_TASK_RUN);
        failed.state = crate::agent_task_lifecycle::AgentTaskRunState::Failed;
        let without_plan = project_record(&failed, None).expect("project");
        assert_eq!(
            eligibility(&without_plan, ControlPlaneAction::Retry),
            ControlPlaneActionAvailability::Indeterminate
        );
        let with_plan = project_record(&failed, Some(&AgentTaskPlan::new("plan", Vec::new())))
            .expect("project");
        assert_eq!(
            eligibility(&with_plan, ControlPlaneAction::Retry),
            ControlPlaneActionAvailability::Unavailable
        );
    }

    #[test]
    fn mission_alias_is_not_accepted_as_a_run_id() {
        let error = service()
            .run(&RunId::new(AGENT_TASK_COOK).expect("Cook alias"))
            .expect_err("mission alias must use a mission resource");
        assert_eq!(error.class, ControlPlaneErrorClass::NotFound);
    }

    #[test]
    fn unknown_run_is_typed_not_found() {
        let error = service()
            .run(&RunId::new("no-such-run").expect("run id"))
            .expect_err("missing");
        assert_eq!(error.class, ControlPlaneErrorClass::NotFound);
        assert!(!error.retryable);
    }

    #[test]
    fn project_record_matches_service_run() {
        let seeded = record(AGENT_TASK_RUN);
        let requested = RunId::new(AGENT_TASK_RUN).expect("run id");
        let from_record = project_record(&seeded, None).expect("project");
        let from_service = service().run(&requested).expect("run");
        assert_eq!(from_record, from_service);
    }
}
