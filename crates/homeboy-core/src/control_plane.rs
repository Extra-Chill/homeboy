//! Control-plane HTTP provider hook.
//!
//! Core owns the versioned HTTP adapter. The orchestration service that
//! assembles [`ControlPlaneRun`] lives in the agent-task layer and registers
//! here so core stays agent-task-agnostic.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock, RwLock};

use homeboy_control_plane_contract::{
    ControlPlaneActionAcknowledgement, ControlPlaneActionOutcome, ControlPlaneActionPayload,
    ControlPlaneActionRequest, ControlPlaneAttempt, ControlPlaneAttemptListRequest,
    ControlPlaneAttemptPage, ControlPlaneCapabilities, ControlPlaneError, ControlPlaneEvent,
    ControlPlaneEventAppendRequest, ControlPlaneEventPage, ControlPlaneEventRetention,
    ControlPlaneEventSource, ControlPlaneExecution, ControlPlaneExecutionPage, ControlPlaneMission,
    ControlPlaneMissionListRequest, ControlPlaneMissionPage, ControlPlaneOperation,
    ControlPlaneReference, ControlPlaneReferencePage, ControlPlaneReferenceRegistration,
    ControlPlaneReferenceType, ControlPlaneRun, ControlPlaneRunListRequest, ControlPlaneRunPage,
    ControlPlaneRunReview, ControlPlaneRunReviewRequest, ControlPlaneSubmissionAcknowledgement,
    ControlPlaneSubmissionRequest, ControlPlaneTask, ControlPlaneTaskListRequest,
    ControlPlaneTaskPage, EventCursor, ExecutionId, MissionId, ReferenceId, RunId, TaskId,
    CONTROL_PLANE_EVENT_APPEND_REQUEST_SCHEMA,
};

use crate::observation::{ControlPlaneActionClaim, ObservationStore, RunRecord};

/// Runtime-neutral result returned by a domain-owned action implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlPlaneActionDelegateResult {
    pub outcome: ControlPlaneActionOutcome,
    pub result: ControlPlaneActionPayload,
    pub message: Option<String>,
}

/// Executes and reconciles actions for one opaque observation run kind.
///
/// Core owns action claims and acknowledgements. Implementations own only the
/// domain effect and the authoritative reconciliation needed after a crash.
pub trait ControlPlaneActionDelegate: Send + Sync {
    fn run_kind(&self) -> &'static str;

    fn execute(
        &self,
        run: &RunRecord,
        request: &ControlPlaneActionRequest,
    ) -> Result<ControlPlaneActionDelegateResult, ControlPlaneError>;

    fn recover(
        &self,
        run: &RunRecord,
        request: &ControlPlaneActionRequest,
    ) -> Result<ControlPlaneActionDelegateResult, ControlPlaneError>;
}

static ACTION_DELEGATES: OnceLock<
    RwLock<BTreeMap<&'static str, Arc<dyn ControlPlaneActionDelegate>>>,
> = OnceLock::new();

/// Register the domain implementation for one durable observation run kind.
pub fn register_control_plane_action_delegate(delegate: Arc<dyn ControlPlaneActionDelegate>) {
    ACTION_DELEGATES
        .get_or_init(|| RwLock::new(BTreeMap::new()))
        .write()
        .expect("control-plane action delegate registry poisoned")
        .insert(delegate.run_kind(), delegate);
}

/// Execute a registered domain action through the kernel-owned claim ledger.
///
/// `None` means no delegate owns this run kind. A completed claim always
/// returns its immutable stored acknowledgement without invoking the domain.
pub fn execute_delegated_action(
    store: &ObservationStore,
    run: &RunRecord,
    request: &ControlPlaneActionRequest,
    project: impl FnOnce() -> Result<ControlPlaneRun, ControlPlaneError>,
) -> Result<Option<ControlPlaneActionAcknowledgement>, ControlPlaneError> {
    let delegate = ACTION_DELEGATES
        .get_or_init(|| RwLock::new(BTreeMap::new()))
        .read()
        .expect("control-plane action delegate registry poisoned")
        .get(run.kind.as_str())
        .cloned();
    let Some(delegate) = delegate else {
        return Ok(None);
    };
    let requested_id = RunId::new(&run.id)
        .map_err(|error| ControlPlaneError::invalid_argument(error.to_string()))?;
    let request_json = serde_json::to_vec(request)
        .map_err(|error| ControlPlaneError::invalid_argument(error.to_string()))?;
    let action = serde_json::to_value(request.action)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .ok_or_else(|| ControlPlaneError::invalid_argument("action has no wire identity"))?;
    let idempotency_digest = homeboy_engine_primitives::content_hash::sha256_hex(
        format!("{}\0{}", action, request.idempotency_key).as_bytes(),
    );
    let request_digest =
        homeboy_engine_primitives::content_hash::sha256_hex(request_json.as_slice());
    let claim = store
        .claim_control_plane_action(&requested_id, &idempotency_digest, &request_digest)
        .map_err(map_store_error)?;
    let precondition_failed = request
        .expected_updated_at
        .as_deref()
        .is_some_and(|expected| {
            Some(expected) != run.finished_at.as_deref().or(Some(run.started_at.as_str()))
        });
    let precondition_result = || ControlPlaneActionDelegateResult {
        outcome: ControlPlaneActionOutcome::Failed,
        result: ControlPlaneActionPayload::empty(),
        message: Some("run changed since the supplied precondition".to_string()),
    };
    let (accepted_at, domain) = match claim {
        ControlPlaneActionClaim::Completed(acknowledgement) => {
            ensure_delegated_action_events(store, request, &acknowledgement)?;
            return Ok(Some(acknowledgement));
        }
        ControlPlaneActionClaim::InProgress => {
            return Err(ControlPlaneError::unavailable(
                "this idempotent action is already in progress",
            ));
        }
        ControlPlaneActionClaim::Acquired { accepted_at } => {
            append_delegated_action_event(
                store,
                &requested_id,
                request,
                "action.accepted",
                &accepted_at,
                serde_json::json!({ "action": request.action }),
            )?;
            let result = if precondition_failed {
                Ok(precondition_result())
            } else {
                delegate.execute(run, request)
            };
            (accepted_at, result)
        }
        ControlPlaneActionClaim::Recover { accepted_at } => {
            append_delegated_action_event(
                store,
                &requested_id,
                request,
                "action.accepted",
                &accepted_at,
                serde_json::json!({ "action": request.action }),
            )?;
            let result = if precondition_failed {
                Ok(precondition_result())
            } else {
                delegate.recover(run, request)
            };
            (accepted_at, result)
        }
    };
    let domain = domain.unwrap_or_else(|error| ControlPlaneActionDelegateResult {
        outcome: ControlPlaneActionOutcome::Failed,
        result: ControlPlaneActionPayload::empty(),
        message: Some(error.message),
    });
    let acknowledgement = ControlPlaneActionAcknowledgement {
        schema: homeboy_control_plane_contract::CONTROL_PLANE_ACTION_ACKNOWLEDGEMENT_SCHEMA
            .to_string(),
        acknowledgement: format!("{}:action:{}:{}", run.id, action, request.idempotency_key),
        run: requested_id.clone(),
        action: request.action,
        idempotency_key: request.idempotency_key.clone(),
        actor: request.actor.clone(),
        accepted_at,
        completed_at: chrono::Utc::now().to_rfc3339(),
        outcome: domain.outcome,
        resource: project()?,
        result: domain.result,
        message: domain.message,
    };
    let acknowledgement = store
        .complete_control_plane_action(&requested_id, &idempotency_digest, &acknowledgement)
        .map_err(map_store_error)?;
    ensure_delegated_action_events(store, request, &acknowledgement)?;
    Ok(Some(acknowledgement))
}

fn ensure_delegated_action_events(
    store: &ObservationStore,
    request: &ControlPlaneActionRequest,
    acknowledgement: &ControlPlaneActionAcknowledgement,
) -> Result<(), ControlPlaneError> {
    append_delegated_action_event(
        store,
        &acknowledgement.run,
        request,
        "action.accepted",
        &acknowledgement.accepted_at,
        serde_json::json!({ "action": request.action }),
    )?;
    append_delegated_action_event(
        store,
        &acknowledgement.run,
        request,
        match acknowledgement.outcome {
            ControlPlaneActionOutcome::Succeeded => "action.succeeded",
            ControlPlaneActionOutcome::AlreadySatisfied => "action.already_satisfied",
            ControlPlaneActionOutcome::Failed => "action.failed",
        },
        &acknowledgement.completed_at,
        serde_json::json!({
            "action": request.action,
            "acknowledgement": acknowledgement.acknowledgement,
            "outcome": acknowledgement.outcome,
        }),
    )
}

fn append_delegated_action_event(
    store: &ObservationStore,
    run: &RunId,
    action: &ControlPlaneActionRequest,
    kind: &str,
    occurred_at: &str,
    data: serde_json::Value,
) -> Result<(), ControlPlaneError> {
    let identity = homeboy_engine_primitives::content_hash::sha256_hex(
        format!(
            "{}\0{:?}\0{}\0{}",
            run, action.action, action.idempotency_key, kind
        )
        .as_bytes(),
    );
    let request = ControlPlaneEventAppendRequest {
        schema: CONTROL_PLANE_EVENT_APPEND_REQUEST_SCHEMA.to_string(),
        idempotency_key: format!("control-plane-action:{identity}"),
        actor: action.actor.clone(),
        kind: kind.to_string(),
        source: ControlPlaneEventSource {
            component: "control-plane".to_string(),
            instance: None,
        },
        occurred_at: Some(occurred_at.to_string()),
        task: None,
        attempt: None,
        execution: None,
        data,
        artifacts: Vec::new(),
        evidence: Vec::new(),
    };
    let request_digest = homeboy_engine_primitives::content_hash::sha256_hex(
        serde_json::to_vec(&request)
            .map_err(|error| ControlPlaneError::invalid_argument(error.to_string()))?
            .as_slice(),
    );
    store
        .append_control_plane_event(run, &request, &identity, &request_digest)
        .map(|_| ())
        .map_err(map_store_error)
}

fn map_store_error(error: crate::Error) -> ControlPlaneError {
    if error.code == crate::ErrorCode::ValidationInvalidArgument {
        ControlPlaneError::invalid_argument(error.message)
    } else {
        ControlPlaneError::unavailable(error.message)
    }
}

/// Supplies control-plane capabilities and resource reads to the HTTP adapter.
pub trait ControlPlaneProvider: Send + Sync {
    fn capabilities(&self) -> ControlPlaneCapabilities {
        ControlPlaneCapabilities::new(Vec::new(), vec![ControlPlaneOperation::GetCapabilities])
    }

    /// Bind an installed extension to the canonical task whose domain plan
    /// selected it. Identity coherence alone does not grant execution authority.
    fn authorize_extension_execution(
        &self,
        _extension_id: &str,
        _run: &RunId,
        _task: &TaskId,
    ) -> Result<bool, ControlPlaneError> {
        Ok(false)
    }

    fn run(&self, requested_id: &RunId) -> Result<ControlPlaneRun, ControlPlaneError> {
        Err(ControlPlaneError::not_found(format!(
            "control-plane run not found: {requested_id}"
        )))
    }

    fn mission(&self, requested_id: &MissionId) -> Result<ControlPlaneMission, ControlPlaneError> {
        Err(ControlPlaneError::not_found(format!(
            "control-plane mission not found: {requested_id}"
        )))
    }

    fn missions(
        &self,
        _request: &ControlPlaneMissionListRequest,
    ) -> Result<ControlPlaneMissionPage, ControlPlaneError> {
        Err(ControlPlaneError::unavailable(
            "control-plane mission discovery is unavailable",
        ))
    }

    fn runs(
        &self,
        _request: &ControlPlaneRunListRequest,
    ) -> Result<ControlPlaneRunPage, ControlPlaneError> {
        Err(ControlPlaneError::unavailable(
            "control-plane run discovery is unavailable",
        ))
    }

    fn task(&self, run: &RunId, task: &TaskId) -> Result<ControlPlaneTask, ControlPlaneError> {
        Err(ControlPlaneError::not_found(format!(
            "control-plane task not found in run {run}: {task}"
        )))
    }

    fn tasks(
        &self,
        _run: &RunId,
        _request: &ControlPlaneTaskListRequest,
    ) -> Result<ControlPlaneTaskPage, ControlPlaneError> {
        Err(ControlPlaneError::unavailable(
            "control-plane task discovery is unavailable",
        ))
    }

    fn attempt(
        &self,
        run: &RunId,
        task: &TaskId,
        attempt_number: u32,
    ) -> Result<ControlPlaneAttempt, ControlPlaneError> {
        Err(ControlPlaneError::not_found(format!(
            "control-plane attempt not found for run {run}, task {task}: {attempt_number}"
        )))
    }

    fn attempts(
        &self,
        _run: &RunId,
        _task: &TaskId,
        _request: &ControlPlaneAttemptListRequest,
    ) -> Result<ControlPlaneAttemptPage, ControlPlaneError> {
        Err(ControlPlaneError::unavailable(
            "control-plane attempt discovery is unavailable",
        ))
    }

    fn execution(
        &self,
        run: &RunId,
        task: &TaskId,
        attempt_number: u32,
        execution: &ExecutionId,
    ) -> Result<ControlPlaneExecution, ControlPlaneError> {
        Err(ControlPlaneError::not_found(format!(
            "control-plane execution not found for run {run}, task {task}, attempt {attempt_number}: {execution}"
        )))
    }

    fn executions(
        &self,
        _run: &RunId,
        _task: &TaskId,
        _attempt_number: u32,
    ) -> Result<ControlPlaneExecutionPage, ControlPlaneError> {
        Err(ControlPlaneError::unavailable(
            "control-plane execution discovery is unavailable",
        ))
    }

    fn reference(
        &self,
        run: &RunId,
        reference_type: ControlPlaneReferenceType,
        reference: &ReferenceId,
    ) -> Result<ControlPlaneReference, ControlPlaneError> {
        Err(ControlPlaneError::not_found(format!(
            "control-plane {reference_type:?} reference not found in run {run}: {reference}"
        )))
    }

    fn references(
        &self,
        _run: &RunId,
        _reference_type: ControlPlaneReferenceType,
    ) -> Result<ControlPlaneReferencePage, ControlPlaneError> {
        Err(ControlPlaneError::unavailable(
            "control-plane reference discovery is unavailable",
        ))
    }

    fn register_reference(
        &self,
        _run: &RunId,
        _reference_type: ControlPlaneReferenceType,
        _request: &ControlPlaneReferenceRegistration,
    ) -> Result<ControlPlaneReference, ControlPlaneError> {
        Err(ControlPlaneError::unavailable(
            "control-plane reference registration is unavailable",
        ))
    }

    fn submit(
        &self,
        _request: &ControlPlaneSubmissionRequest,
    ) -> Result<ControlPlaneSubmissionAcknowledgement, ControlPlaneError> {
        Err(ControlPlaneError::unavailable(
            "control-plane run submission is unavailable",
        ))
    }

    fn events(
        &self,
        requested_id: &RunId,
        _cursor: Option<&EventCursor>,
    ) -> Result<ControlPlaneEventPage, ControlPlaneError> {
        Err(ControlPlaneError::not_found(format!(
            "control-plane run not found: {requested_id}"
        )))
    }

    fn append_event(
        &self,
        requested_id: &RunId,
        _request: &ControlPlaneEventAppendRequest,
    ) -> Result<ControlPlaneEvent, ControlPlaneError> {
        Err(ControlPlaneError::not_found(format!(
            "control-plane run not found: {requested_id}"
        )))
    }

    fn event_retention(
        &self,
        requested_id: &RunId,
    ) -> Result<ControlPlaneEventRetention, ControlPlaneError> {
        Err(ControlPlaneError::not_found(format!(
            "control-plane run not found: {requested_id}"
        )))
    }

    fn review(
        &self,
        requested_id: &RunId,
        _request: &ControlPlaneRunReviewRequest,
    ) -> Result<ControlPlaneRunReview, ControlPlaneError> {
        Err(ControlPlaneError::not_found(format!(
            "control-plane run not found: {requested_id}"
        )))
    }

    fn execute_action(
        &self,
        requested_id: &RunId,
        _request: &ControlPlaneActionRequest,
    ) -> Result<ControlPlaneActionAcknowledgement, ControlPlaneError> {
        Err(ControlPlaneError::not_found(format!(
            "control-plane run not found: {requested_id}"
        )))
    }
}

struct NoopProvider;

impl ControlPlaneProvider for NoopProvider {}

homeboy_engine_primitives::provider_registry! {
    provider: dyn ControlPlaneProvider,
    noop: NoopProvider,
    /// Register the control-plane orchestration provider. Called once at
    /// startup by the agent-task layer.
    register: pub fn register_control_plane_provider,
    /// Run `f` against the registered provider, or the no-op provider if none
    /// is registered.
    with: fn with_provider,
}

pub fn capabilities() -> ControlPlaneCapabilities {
    with_provider(|provider| provider.capabilities())
}

pub fn authorize_extension_execution(
    extension_id: &str,
    run: &RunId,
    task: &TaskId,
) -> Result<bool, ControlPlaneError> {
    with_provider(|provider| provider.authorize_extension_execution(extension_id, run, task))
}

pub fn run(requested_id: &RunId) -> Result<ControlPlaneRun, ControlPlaneError> {
    with_provider(|provider| provider.run(requested_id))
}

pub fn mission(requested_id: &MissionId) -> Result<ControlPlaneMission, ControlPlaneError> {
    with_provider(|provider| provider.mission(requested_id))
}

pub fn missions(
    request: &ControlPlaneMissionListRequest,
) -> Result<ControlPlaneMissionPage, ControlPlaneError> {
    request.validate()?;
    with_provider(|provider| provider.missions(request))
}

pub fn runs(
    request: &ControlPlaneRunListRequest,
) -> Result<ControlPlaneRunPage, ControlPlaneError> {
    request.validate()?;
    with_provider(|provider| provider.runs(request))
}

pub fn task(run: &RunId, task: &TaskId) -> Result<ControlPlaneTask, ControlPlaneError> {
    with_provider(|provider| provider.task(run, task))
}

pub fn tasks(
    run: &RunId,
    request: &ControlPlaneTaskListRequest,
) -> Result<ControlPlaneTaskPage, ControlPlaneError> {
    request.validate()?;
    with_provider(|provider| provider.tasks(run, request))
}

pub fn attempt(
    run: &RunId,
    task: &TaskId,
    attempt_number: u32,
) -> Result<ControlPlaneAttempt, ControlPlaneError> {
    with_provider(|provider| provider.attempt(run, task, attempt_number))
}

pub fn attempts(
    run: &RunId,
    task: &TaskId,
    request: &ControlPlaneAttemptListRequest,
) -> Result<ControlPlaneAttemptPage, ControlPlaneError> {
    request.validate()?;
    with_provider(|provider| provider.attempts(run, task, request))
}

pub fn execution(
    run: &RunId,
    task: &TaskId,
    attempt_number: u32,
    execution: &ExecutionId,
) -> Result<ControlPlaneExecution, ControlPlaneError> {
    with_provider(|provider| provider.execution(run, task, attempt_number, execution))
}

pub fn executions(
    run: &RunId,
    task: &TaskId,
    attempt_number: u32,
) -> Result<ControlPlaneExecutionPage, ControlPlaneError> {
    with_provider(|provider| provider.executions(run, task, attempt_number))
}

pub fn reference(
    run: &RunId,
    reference_type: ControlPlaneReferenceType,
    reference: &ReferenceId,
) -> Result<ControlPlaneReference, ControlPlaneError> {
    with_provider(|provider| provider.reference(run, reference_type, reference))
}

pub fn references(
    run: &RunId,
    reference_type: ControlPlaneReferenceType,
) -> Result<ControlPlaneReferencePage, ControlPlaneError> {
    with_provider(|provider| provider.references(run, reference_type))
}

pub fn register_reference(
    run: &RunId,
    reference_type: ControlPlaneReferenceType,
    request: &ControlPlaneReferenceRegistration,
) -> Result<ControlPlaneReference, ControlPlaneError> {
    request.validate()?;
    with_provider(|provider| provider.register_reference(run, reference_type, request))
}

pub fn submit(
    request: &ControlPlaneSubmissionRequest,
) -> Result<ControlPlaneSubmissionAcknowledgement, ControlPlaneError> {
    request.validate()?;
    with_provider(|provider| provider.submit(request))
}

pub fn events(
    requested_id: &RunId,
    cursor: Option<&EventCursor>,
) -> Result<ControlPlaneEventPage, ControlPlaneError> {
    with_provider(|provider| provider.events(requested_id, cursor))
}

pub fn append_event(
    requested_id: &RunId,
    request: &ControlPlaneEventAppendRequest,
) -> Result<ControlPlaneEvent, ControlPlaneError> {
    request.validate()?;
    with_provider(|provider| provider.append_event(requested_id, request))
}

pub fn event_retention(
    requested_id: &RunId,
) -> Result<ControlPlaneEventRetention, ControlPlaneError> {
    with_provider(|provider| provider.event_retention(requested_id))
}

pub fn review(
    requested_id: &RunId,
    request: &ControlPlaneRunReviewRequest,
) -> Result<ControlPlaneRunReview, ControlPlaneError> {
    request.validate()?;
    with_provider(|provider| provider.review(requested_id, request))
}

pub fn execute_action(
    requested_id: &RunId,
    request: &ControlPlaneActionRequest,
) -> Result<ControlPlaneActionAcknowledgement, ControlPlaneError> {
    request.validate()?;
    with_provider(|provider| provider.execute_action(requested_id, request))
}

#[cfg(test)]
mod tests {
    use super::{append_delegated_action_event, ControlPlaneProvider, NoopProvider};
    use homeboy_control_plane_contract::{
        ControlPlaneAction, ControlPlaneActionPayload, ControlPlaneActionRequest,
        ControlPlaneOperation, RunId, CONTROL_PLANE_ACTION_REQUEST_SCHEMA,
    };

    #[test]
    fn noop_provider_advertises_discovery_without_run_reads() {
        let capabilities = NoopProvider.capabilities();
        assert!(capabilities.resources.is_empty());
        assert_eq!(
            capabilities.operations,
            vec![ControlPlaneOperation::GetCapabilities]
        );
    }

    #[test]
    fn distinct_actions_may_share_a_caller_idempotency_key() {
        let directory = tempfile::tempdir().unwrap();
        let store = crate::observation::ObservationStore::open_initialized_at(
            directory.path().join("store.sqlite"),
        )
        .unwrap();
        store
            .start_run_with_id(
                crate::observation::NewRunRecord::builder("test").build(),
                "run-1".to_string(),
            )
            .unwrap();
        let run = RunId::new("run-1").unwrap();
        for action in [ControlPlaneAction::Resume, ControlPlaneAction::Reconcile] {
            append_delegated_action_event(
                &store,
                &run,
                &ControlPlaneActionRequest {
                    schema: CONTROL_PLANE_ACTION_REQUEST_SCHEMA.to_string(),
                    action,
                    idempotency_key: "same-key".to_string(),
                    actor: "test".to_string(),
                    expected_updated_at: None,
                    parameters: ControlPlaneActionPayload::empty(),
                    confirmed: false,
                },
                "action.accepted",
                "now",
                serde_json::json!({ "action": action }),
            )
            .unwrap();
        }
        assert_eq!(
            store
                .control_plane_event_stream(&run)
                .unwrap()
                .unwrap()
                .len(),
            2
        );
    }
}
