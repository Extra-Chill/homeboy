//! Public Homeboy orchestration service.
//!
//! Owns capabilities and run retrieval. The local CLI and the daemon HTTP
//! adapter call this service; they do not assemble a second control-plane
//! projection. Construct with an explicit lookup — the service does not
//! resolve ambient stores or providers itself.

use homeboy_control_plane_contract::{
    ControlPlaneAction, ControlPlaneActionAcknowledgement, ControlPlaneActionOutcome,
    ControlPlaneActionPayload, ControlPlaneActionRequest, ControlPlaneBlocker,
    ControlPlaneCancelParameters, ControlPlaneCapabilities, ControlPlaneError,
    ControlPlaneErrorClass, ControlPlaneEvidenceRef, ControlPlaneLocation, ControlPlaneOperation,
    ControlPlaneOwner, ControlPlaneProviderSummary, ControlPlaneResource, ControlPlaneRun,
    ControlPlaneRunState, ControlPlaneRuntime, ControlPlaneStateSummary, ExecutionId,
    ProviderSessionId, RunId, CONTROL_PLANE_ACTION_ACKNOWLEDGEMENT_SCHEMA,
    CONTROL_PLANE_ACTION_REQUEST_SCHEMA, CONTROL_PLANE_CANCEL_PARAMETERS_SCHEMA,
    CONTROL_PLANE_EMPTY_ACTION_PAYLOAD_SCHEMA,
};
use homeboy_core::control_plane::{register_control_plane_provider, ControlPlaneProvider};

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
const ACTION_LEASE: std::time::Duration = std::time::Duration::from_secs(30);

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

impl OrchestrationService<LifecycleStoreLookup> {
    /// Operations wired by the durable lifecycle-backed provider.
    pub fn capabilities() -> ControlPlaneCapabilities {
        let mut capabilities = Self::read_capabilities();
        capabilities
            .operations
            .push(ControlPlaneOperation::ExecuteRunAction);
        capabilities
    }

    /// Execute the canonical run mutation against the same durable lifecycle
    /// store used by status and events.
    pub fn execute_action(
        &self,
        requested_id: &RunId,
        request: &ControlPlaneActionRequest,
    ) -> Result<ControlPlaneActionAcknowledgement, ControlPlaneError> {
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
                            ControlPlaneActionPayload::empty(),
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
                                Ok(cancelled) => (
                                    ControlPlaneActionOutcome::Succeeded,
                                    project_record(&cancelled, None)?,
                                    ControlPlaneActionPayload::empty(),
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
                        ControlPlaneAction::Reconcile => {
                            match crate::agent_task_service::reconcile_run(
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
                        _ => {
                            return Err(ControlPlaneError::invalid_argument(format!(
                                "{} is not wired through the canonical action executor",
                                action_name(request.action)
                            )))
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
        ControlPlaneAction::Reconcile => CONTROL_PLANE_EMPTY_ACTION_PAYLOAD_SCHEMA,
        _ => {
            return Err(ControlPlaneError::invalid_argument(format!(
                "{} is not wired through the canonical action executor",
                action_name(request.action)
            )))
        }
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
    if matches!(request.action, ControlPlaneAction::Cancel) && !request.confirmed {
        return Err(ControlPlaneError::invalid_argument(
            "cancel requires explicit confirmation",
        ));
    }
    Ok(())
}

const fn action_name(action: ControlPlaneAction) -> &'static str {
    match action {
        ControlPlaneAction::Cancel => "cancel",
        ControlPlaneAction::Resume => "resume",
        ControlPlaneAction::Retry => "retry",
        ControlPlaneAction::Review => "review",
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

pub fn execute_action_from_current_environment(
    run_id: &str,
    request: &ControlPlaneActionRequest,
) -> homeboy_core::Result<ControlPlaneActionAcknowledgement> {
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
        .execute_action(&requested_id, request)
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
    resource.candidate = candidate(record);
    resource.gates = gates(record);
    resource.publication = publication(record);
    resource.action_eligibility = Some(lifecycle_action_eligibility(record, plan));
    resource.created_at = record.submitted_at.clone();
    resource.updated_at = record.updated_at.clone();
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
    record
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
        })
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
        event_page, project_record, LifecycleStoreLookup, OrchestrationService, RunLookup,
        RunSnapshot,
    };
    use crate::agent_task_lifecycle::{AgentTaskLifecycleStore, AgentTaskRunRecord};
    use crate::agent_task_schedule::AgentTaskPlan;
    use homeboy_control_plane_contract::{
        ControlPlaneAction, ControlPlaneActionAvailability, ControlPlaneActionOutcome,
        ControlPlaneActionPayload, ControlPlaneActionRequest, ControlPlaneErrorClass,
        ControlPlaneEvent, ControlPlaneEventSource, ControlPlaneOperation, ControlPlaneRunState,
        EventCursor, EventId, RunId, CONTROL_PLANE_ACTION_ELIGIBILITY_SCHEMA,
        CONTROL_PLANE_ACTION_REQUEST_SCHEMA, CONTROL_PLANE_CANCEL_PARAMETERS_SCHEMA,
        CONTROL_PLANE_EVENT_SCHEMA, CONTROL_PLANE_RUN_SCHEMA,
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
                ControlPlaneOperation::ExecuteRunAction,
            ]
        );
        assert!(!capabilities.operations.is_empty());
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
            eligibility(&resource, ControlPlaneAction::Review),
            ControlPlaneActionAvailability::Available
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
