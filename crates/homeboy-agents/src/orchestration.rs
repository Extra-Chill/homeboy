//! Public Homeboy orchestration service.
//!
//! Owns capabilities and run retrieval. The local CLI and the daemon HTTP
//! adapter call this service; they do not assemble a second control-plane
//! projection. Construct with an explicit lookup — the service does not
//! resolve ambient stores or providers itself.

use base64::Engine;
use chrono::{DateTime, Utc};
use homeboy_control_plane_contract::{
    AttemptCursor, AttemptId, ControlPlaneAction, ControlPlaneActionAcknowledgement,
    ControlPlaneActionAvailability, ControlPlaneActionOutcome, ControlPlaneActionPayload,
    ControlPlaneActionRequest, ControlPlaneAdmissionRetry, ControlPlaneAdmissionRetryDisposition,
    ControlPlaneAttempt, ControlPlaneAttemptListRequest, ControlPlaneAttemptPage,
    ControlPlaneBlocker, ControlPlaneCancelDisposition, ControlPlaneCancelParameters,
    ControlPlaneCancelResult, ControlPlaneCapabilities, ControlPlaneError, ControlPlaneErrorClass,
    ControlPlaneEventAppendRequest, ControlPlaneEventRetention, ControlPlaneEventSource,
    ControlPlaneEvidenceRef, ControlPlaneExecution, ControlPlaneExecutionPage,
    ControlPlaneLiveness, ControlPlaneLocation, ControlPlaneMission,
    ControlPlaneMissionListRequest, ControlPlaneMissionPage, ControlPlaneOperation,
    ControlPlaneOwner, ControlPlaneProviderSummary, ControlPlaneReference,
    ControlPlaneReferencePage, ControlPlaneReferenceRegistration, ControlPlaneReferenceType,
    ControlPlaneResource, ControlPlaneRun, ControlPlaneRunListRequest, ControlPlaneRunPage,
    ControlPlaneRunPlacement, ControlPlaneRunPlacementEffective, ControlPlaneRunPlacementRequested,
    ControlPlaneRunPlacementSelected, ControlPlaneRunReview, ControlPlaneRunReviewRequest,
    ControlPlaneRunState, ControlPlaneRuntime, ControlPlaneState, ControlPlaneStateSummary,
    ControlPlaneSubmissionAcknowledgement, ControlPlaneSubmissionRequest, ControlPlaneTask,
    ControlPlaneTaskListRequest, ControlPlaneTaskPage, EventCursor, ExecutionId, MissionCursor,
    MissionId, ProviderSessionId, ReferenceId, RunCursor, RunId, TaskCursor, TaskId,
    CONTROL_PLANE_ACTION_ACKNOWLEDGEMENT_SCHEMA, CONTROL_PLANE_ATTEMPT_PAGE_SCHEMA,
    CONTROL_PLANE_ATTEMPT_SCHEMA, CONTROL_PLANE_CANCEL_RESULT_SCHEMA,
    CONTROL_PLANE_EVENT_APPEND_REQUEST_SCHEMA, CONTROL_PLANE_EVENT_RETENTION_SCHEMA,
    CONTROL_PLANE_EXECUTION_PAGE_SCHEMA, CONTROL_PLANE_EXECUTION_SCHEMA,
    CONTROL_PLANE_MISSION_PAGE_SCHEMA, CONTROL_PLANE_MISSION_SCHEMA,
    CONTROL_PLANE_PLACEMENT_UPDATE_RESULT_SCHEMA, CONTROL_PLANE_PROMOTE_RESULT_SCHEMA,
    CONTROL_PLANE_QUARANTINE_RESULT_SCHEMA, CONTROL_PLANE_REARM_RESULT_SCHEMA,
    CONTROL_PLANE_REFERENCE_PAGE_SCHEMA, CONTROL_PLANE_REFERENCE_SCHEMA,
    CONTROL_PLANE_RESUME_RESULT_SCHEMA, CONTROL_PLANE_RETRY_RESULT_SCHEMA,
    CONTROL_PLANE_RUN_PAGE_SCHEMA, CONTROL_PLANE_TASK_PAGE_SCHEMA, CONTROL_PLANE_TASK_SCHEMA,
};
use homeboy_control_plane_contract::{
    ControlPlanePlacementUpdateParameters, ControlPlaneQuarantineParameters,
    ControlPlaneRetryParameters,
};
use homeboy_core::control_plane::{register_control_plane_provider, ControlPlaneProvider};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::agent_task_lifecycle::{
    canonical_control_plane_identities, claim_operation_with_intent_in_store,
    complete_cook_operation_in_store, lifecycle_action_eligibility, now_timestamp,
    operation_claim_in_store, operation_lease_is_active_in_store, resolve_run_id_in_store,
    AgentTaskLifecycleStore, AgentTaskRunRecord, AgentTaskRunState,
    CanonicalControlPlaneIdentities, ClaimOutcome,
};
use crate::agent_task_schedule::AgentTaskPlan;

const ID_BOUND: usize = 128;
const STATE_BOUND: usize = 64;
const MESSAGE_BOUND: usize = 256;
const GATE_BOUND: usize = 12;
pub(crate) const REF_BOUND: usize = 32;
const REGISTERED_REFERENCE_BOUND: usize = 100;
const URI_BOUND: usize = 512;
const EVENT_PAGE_BOUND: usize = 100;
const REVIEW_EVIDENCE_BOUND: usize = 1024 * 1024;
const REVIEW_EVIDENCE_FIELD_BOUND: usize = 256 * 1024;
const ACTION_LEASE: std::time::Duration = std::time::Duration::from_secs(30);
const CANCEL_TERMINAL_WAIT: Duration = Duration::from_secs(15);
const CANCEL_TERMINAL_POLL_INTERVAL: Duration = Duration::from_secs(1);
const RUN_CURSOR_SCHEMA: &str = "homeboy/control-plane-run-cursor/v1";
const MISSION_CURSOR_SCHEMA: &str = "homeboy/control-plane-mission-cursor/v1";
const TASK_CURSOR_SCHEMA: &str = "homeboy/control-plane-task-cursor/v1";
const ATTEMPT_CURSOR_SCHEMA: &str = "homeboy/control-plane-attempt-cursor/v1";
const EVENT_CURSOR_SCHEMA: &str = "homeboy/control-plane-event-cursor/v1";
const INTERNAL_ACTION_EVENT_KEY_PREFIX: &str = "homeboy-internal-action:";
const RUN_CURSOR_BOUND: usize = 1024;

/// One bounded non-reconciling read of the durable record and optional plan.
#[derive(Debug, Clone)]
pub struct RunSnapshot {
    pub record: AgentTaskRunRecord,
    pub plan: Option<AgentTaskPlan>,
}

#[derive(Debug, Clone)]
pub struct RunPagePosition {
    pub started_at: String,
    pub run_id: String,
}

pub struct RunSnapshotPage {
    pub snapshots: Vec<RunSnapshot>,
    pub next_position: Option<RunPagePosition>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunCursorPayload {
    schema: String,
    started_at: String,
    run_id: String,
    mission_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MissionCursorPayload {
    schema: String,
    created_at: String,
    mission_id: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskCursorPayload {
    schema: String,
    run_id: String,
    task_id: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AttemptCursorPayload {
    schema: String,
    run_id: String,
    task_id: String,
    attempt_number: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EventCursorPayload {
    schema: String,
    run_id: String,
    sequence: u64,
}

/// Lookup used by [`OrchestrationService`]. Callers inject stores or test
/// doubles; the service never opens an environment-rooted store itself.
pub trait RunLookup {
    fn get(&self, id: &RunId) -> Result<Option<RunSnapshot>, ControlPlaneError>;
}

pub trait RunListLookup {
    fn list(
        &self,
        mission: Option<&MissionId>,
        after: Option<&RunPagePosition>,
        limit: usize,
    ) -> Result<RunSnapshotPage, ControlPlaneError>;
}

pub trait MissionLookup {
    fn get_mission(
        &self,
        id: &MissionId,
    ) -> Result<Option<homeboy_core::observation::MissionRecord>, ControlPlaneError>;
    fn list_missions(
        &self,
        after: Option<&homeboy_core::observation::MissionCursor>,
        limit: usize,
    ) -> Result<homeboy_core::observation::MissionPage, ControlPlaneError>;
}

pub trait EventLookup {
    fn events(
        &self,
        id: &RunId,
        cursor: Option<&homeboy_control_plane_contract::EventCursor>,
    ) -> Result<Option<homeboy_control_plane_contract::ControlPlaneEventPage>, ControlPlaneError>;
    fn event_retention(
        &self,
        id: &RunId,
    ) -> Result<Option<ControlPlaneEventRetention>, ControlPlaneError>;
}

/// Durable lifecycle-store lookup. Bounded, non-reconciling, non-writing.
pub struct LifecycleStoreLookup {
    store: AgentTaskLifecycleStore,
}

impl LifecycleStoreLookup {
    pub fn new(store: AgentTaskLifecycleStore) -> Self {
        Self { store }
    }

    fn plan(&self, run_id: &str) -> Result<Option<AgentTaskPlan>, ControlPlaneError> {
        match self.store.read_controller_plan(run_id) {
            Ok(plan) => Ok(Some(plan)),
            Err(error)
                if error.code == homeboy_core::ErrorCode::ValidationInvalidArgument
                    && error
                        .message
                        .contains("unsupported agent-task execution budget version") =>
            {
                Err(ControlPlaneError::invalid_argument(error.message))
            }
            Err(_) => Ok(None),
        }
    }
}

impl RunLookup for LifecycleStoreLookup {
    fn get(&self, id: &RunId) -> Result<Option<RunSnapshot>, ControlPlaneError> {
        let record = match self.store.read_record_bounded(id.as_str()) {
            Ok(record) => record,
            Err(error) if is_run_not_found(&error) => return Ok(None),
            Err(error) => return Err(ControlPlaneError::unavailable(error.message)),
        };
        let plan = self.plan(&record.run_id)?;
        Ok(Some(RunSnapshot { record, plan }))
    }
}

impl RunListLookup for LifecycleStoreLookup {
    fn list(
        &self,
        mission: Option<&MissionId>,
        after: Option<&RunPagePosition>,
        limit: usize,
    ) -> Result<RunSnapshotPage, ControlPlaneError> {
        let after = after.map(|position| homeboy_core::observation::RunCursor {
            started_at: position.started_at.clone(),
            id: position.run_id.clone(),
        });
        let (records, _truncated, next_cursor) = if let Some(mission) = mission {
            self.store
                .read_mission_record_page(mission.as_str(), after, limit)
        } else {
            self.store.read_record_page(after, limit)
        }
        .map_err(map_lifecycle_error)?;
        let snapshots = records
            .into_iter()
            .map(|record| {
                let plan = self.plan(&record.run_id)?;
                Ok(RunSnapshot { record, plan })
            })
            .collect::<Result<Vec<_>, ControlPlaneError>>()?;
        Ok(RunSnapshotPage {
            snapshots,
            next_position: next_cursor.map(|cursor| RunPagePosition {
                started_at: cursor.started_at,
                run_id: cursor.id,
            }),
        })
    }
}

impl MissionLookup for LifecycleStoreLookup {
    fn get_mission(
        &self,
        id: &MissionId,
    ) -> Result<Option<homeboy_core::observation::MissionRecord>, ControlPlaneError> {
        self.store
            .read_mission(id.as_str())
            .map_err(map_lifecycle_error)
    }

    fn list_missions(
        &self,
        after: Option<&homeboy_core::observation::MissionCursor>,
        limit: usize,
    ) -> Result<homeboy_core::observation::MissionPage, ControlPlaneError> {
        self.store
            .read_mission_page(after, limit)
            .map_err(map_lifecycle_error)
    }
}

impl EventLookup for LifecycleStoreLookup {
    fn events(
        &self,
        id: &RunId,
        cursor: Option<&homeboy_control_plane_contract::EventCursor>,
    ) -> Result<Option<homeboy_control_plane_contract::ControlPlaneEventPage>, ControlPlaneError>
    {
        let events = self
            .store
            .open_observation_readonly()
            .map_err(map_lifecycle_error)?
            .control_plane_event_stream(id)
            .map_err(map_lifecycle_error)?;
        events
            .map(|events| event_page(id.clone(), events, cursor))
            .transpose()
    }

    fn event_retention(
        &self,
        id: &RunId,
    ) -> Result<Option<ControlPlaneEventRetention>, ControlPlaneError> {
        self.store
            .open_observation_readonly()
            .map_err(map_lifecycle_error)?
            .control_plane_event_retention(id)
            .map_err(map_lifecycle_error)
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
    pub fn read_capabilities() -> ControlPlaneCapabilities
    where
        L: RunListLookup + MissionLookup,
    {
        ControlPlaneCapabilities::new(
            vec![
                ControlPlaneResource::Mission,
                ControlPlaneResource::Run,
                ControlPlaneResource::Task,
                ControlPlaneResource::Attempt,
                ControlPlaneResource::Execution,
                ControlPlaneResource::Artifact,
                ControlPlaneResource::Evidence,
                ControlPlaneResource::ExternalReference,
                ControlPlaneResource::Event,
            ],
            vec![
                ControlPlaneOperation::GetCapabilities,
                ControlPlaneOperation::ListMissions,
                ControlPlaneOperation::GetMission,
                ControlPlaneOperation::SubmitRun,
                ControlPlaneOperation::ListRuns,
                ControlPlaneOperation::GetRun,
                ControlPlaneOperation::ListRunTasks,
                ControlPlaneOperation::GetRunTask,
                ControlPlaneOperation::ListTaskAttempts,
                ControlPlaneOperation::GetTaskAttempt,
                ControlPlaneOperation::ListAttemptExecutions,
                ControlPlaneOperation::GetAttemptExecution,
                ControlPlaneOperation::ListRunArtifacts,
                ControlPlaneOperation::GetRunArtifact,
                ControlPlaneOperation::RegisterRunArtifact,
                ControlPlaneOperation::ListRunEvidence,
                ControlPlaneOperation::GetRunEvidence,
                ControlPlaneOperation::RegisterRunEvidence,
                ControlPlaneOperation::ListRunExternalReferences,
                ControlPlaneOperation::GetRunExternalReference,
                ControlPlaneOperation::RegisterRunExternalReference,
                ControlPlaneOperation::GetRunEvents,
                ControlPlaneOperation::GetRunEventRetention,
                ControlPlaneOperation::AppendRunEvent,
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

    pub fn task(&self, run: &RunId, task: &TaskId) -> Result<ControlPlaneTask, ControlPlaneError> {
        let snapshot = self
            .lookup
            .get(run)?
            .ok_or_else(|| ControlPlaneError::not_found(format!("run not found: {run}")))?;
        let mut matches = snapshot
            .record
            .tasks
            .iter()
            .filter(|candidate| candidate.task_id == task.as_str());
        let matched = matches.next().ok_or_else(|| {
            ControlPlaneError::not_found(format!("task not found in run {run}: {task}"))
        })?;
        if matches.next().is_some() {
            return Err(ControlPlaneError::invalid_argument(format!(
                "run {run} contains duplicate task identity {task}"
            )));
        }
        project_task(&snapshot.record, matched)
    }

    pub fn tasks(
        &self,
        run: &RunId,
        request: &ControlPlaneTaskListRequest,
    ) -> Result<ControlPlaneTaskPage, ControlPlaneError> {
        request.validate()?;
        let after = request
            .cursor
            .as_ref()
            .map(|cursor| decode_task_cursor(cursor, run))
            .transpose()?;
        let snapshot = self
            .lookup
            .get(run)?
            .ok_or_else(|| ControlPlaneError::not_found(format!("run not found: {run}")))?;
        let mut tasks = snapshot.record.tasks.iter().collect::<Vec<_>>();
        tasks.sort_by(|left, right| left.task_id.cmp(&right.task_id));
        if tasks
            .windows(2)
            .any(|pair| pair[0].task_id == pair[1].task_id)
        {
            return Err(ControlPlaneError::invalid_argument(format!(
                "run {run} contains duplicate task identities"
            )));
        }
        if let Some(after) = after.as_ref() {
            tasks.retain(|task| task.task_id > *after);
        }
        let has_more = tasks.len() > request.limit as usize;
        tasks.truncate(request.limit as usize);
        let projected = tasks
            .into_iter()
            .map(|task| project_task(&snapshot.record, task))
            .collect::<Result<Vec<_>, _>>()?;
        let next_cursor = has_more
            .then(|| {
                projected
                    .last()
                    .expect("nonempty truncated task page")
                    .task
                    .clone()
            })
            .map(|task| encode_task_cursor(run, &task))
            .transpose()?;
        Ok(ControlPlaneTaskPage {
            schema: CONTROL_PLANE_TASK_PAGE_SCHEMA.to_string(),
            run: run.clone(),
            tasks: projected,
            next_cursor,
            has_more,
        })
    }

    pub fn attempt(
        &self,
        run: &RunId,
        task: &TaskId,
        attempt_number: u32,
    ) -> Result<ControlPlaneAttempt, ControlPlaneError> {
        let snapshot = self
            .lookup
            .get(run)?
            .ok_or_else(|| ControlPlaneError::not_found(format!("run not found: {run}")))?;
        provider_attempts(&snapshot.record, task)?
            .into_iter()
            .find(|attempt| attempt.attempt_number == attempt_number)
            .ok_or_else(|| {
                ControlPlaneError::not_found(format!(
                    "attempt not found for run {run}, task {task}: {attempt_number}"
                ))
            })
    }

    pub fn attempts(
        &self,
        run: &RunId,
        task: &TaskId,
        request: &ControlPlaneAttemptListRequest,
    ) -> Result<ControlPlaneAttemptPage, ControlPlaneError> {
        request.validate()?;
        let after = request
            .cursor
            .as_ref()
            .map(|cursor| decode_attempt_cursor(cursor, run, task))
            .transpose()?;
        let snapshot = self
            .lookup
            .get(run)?
            .ok_or_else(|| ControlPlaneError::not_found(format!("run not found: {run}")))?;
        let mut attempts = provider_attempts(&snapshot.record, task)?;
        if let Some(after) = after {
            attempts.retain(|attempt| attempt.attempt_number > after);
        }
        let has_more = attempts.len() > request.limit as usize;
        attempts.truncate(request.limit as usize);
        let next_cursor = has_more
            .then(|| {
                attempts
                    .last()
                    .expect("nonempty truncated attempt page")
                    .attempt_number
            })
            .map(|number| encode_attempt_cursor(run, task, number))
            .transpose()?;
        Ok(ControlPlaneAttemptPage {
            schema: CONTROL_PLANE_ATTEMPT_PAGE_SCHEMA.to_string(),
            run: run.clone(),
            task: task.clone(),
            attempts,
            next_cursor,
            has_more,
        })
    }

    pub fn execution(
        &self,
        run: &RunId,
        task: &TaskId,
        attempt_number: u32,
        requested: &ExecutionId,
    ) -> Result<ControlPlaneExecution, ControlPlaneError> {
        self.executions(run, task, attempt_number)?
            .executions
            .into_iter()
            .find(|execution| execution.execution == *requested)
            .ok_or_else(|| {
                ControlPlaneError::not_found(format!(
                    "execution not found for run {run}, task {task}, attempt {attempt_number}: {requested}"
                ))
            })
    }

    pub fn executions(
        &self,
        run: &RunId,
        task: &TaskId,
        attempt_number: u32,
    ) -> Result<ControlPlaneExecutionPage, ControlPlaneError> {
        let attempt = self.attempt(run, task, attempt_number)?;
        let executions = attempt
            .execution
            .clone()
            .map(|execution| {
                vec![ControlPlaneExecution {
                    schema: CONTROL_PLANE_EXECUTION_SCHEMA.to_string(),
                    run: run.clone(),
                    task: task.clone(),
                    attempt: attempt.attempt.clone(),
                    execution,
                    state: attempt.state,
                    started_at: attempt.started_at.clone(),
                    finished_at: attempt.finished_at.clone(),
                }]
            })
            .unwrap_or_default();
        Ok(ControlPlaneExecutionPage {
            schema: CONTROL_PLANE_EXECUTION_PAGE_SCHEMA.to_string(),
            run: run.clone(),
            task: task.clone(),
            attempt: attempt.attempt,
            executions,
        })
    }

    pub fn reference(
        &self,
        run: &RunId,
        reference_type: ControlPlaneReferenceType,
        requested: &ReferenceId,
    ) -> Result<ControlPlaneReference, ControlPlaneError> {
        self.references(run, reference_type)?
            .references
            .into_iter()
            .find(|reference| reference.reference == *requested)
            .ok_or_else(|| {
                ControlPlaneError::not_found(format!(
                    "{} reference not found in run {run}: {requested}",
                    reference_type_name(reference_type)
                ))
            })
    }

    pub fn references(
        &self,
        run: &RunId,
        reference_type: ControlPlaneReferenceType,
    ) -> Result<ControlPlaneReferencePage, ControlPlaneError> {
        let snapshot = self
            .lookup
            .get(run)?
            .ok_or_else(|| ControlPlaneError::not_found(format!("run not found: {run}")))?;
        Ok(ControlPlaneReferencePage {
            schema: CONTROL_PLANE_REFERENCE_PAGE_SCHEMA.to_string(),
            run: run.clone(),
            reference_type,
            references: references_for_record(&snapshot.record, reference_type)?,
        })
    }
}

impl OrchestrationService<LifecycleStoreLookup> {
    pub fn register_reference(
        &self,
        run: &RunId,
        reference_type: ControlPlaneReferenceType,
        request: &ControlPlaneReferenceRegistration,
    ) -> Result<ControlPlaneReference, ControlPlaneError> {
        request.validate()?;
        register_reference_in_store(&self.lookup.store, run, reference_type, request)
    }
}

fn reference_type_name(reference_type: ControlPlaneReferenceType) -> &'static str {
    match reference_type {
        ControlPlaneReferenceType::Artifact => "artifact",
        ControlPlaneReferenceType::Evidence => "evidence",
        ControlPlaneReferenceType::ExternalReference => "external_reference",
    }
}

fn references_for_record(
    record: &AgentTaskRunRecord,
    reference_type: ControlPlaneReferenceType,
) -> Result<Vec<ControlPlaneReference>, ControlPlaneError> {
    let run = RunId::new(&record.run_id)
        .map_err(|error| ControlPlaneError::invalid_argument(error.to_string()))?;
    let automatic = match reference_type {
        ControlPlaneReferenceType::Artifact => artifact_refs(record),
        ControlPlaneReferenceType::Evidence => evidence_refs(record),
        ControlPlaneReferenceType::ExternalReference => Vec::new(),
    };
    let mut references = automatic
        .into_iter()
        .map(|reference| {
            Ok(ControlPlaneReference {
                schema: CONTROL_PLANE_REFERENCE_SCHEMA.to_string(),
                run: run.clone(),
                reference_type,
                reference: ReferenceId::new(reference.id)
                    .map_err(|error| ControlPlaneError::invalid_argument(error.to_string()))?,
                kind: reference.kind,
                uri: reference.uri,
                registered_at: None,
                actor: None,
            })
        })
        .collect::<Result<Vec<_>, ControlPlaneError>>()?;
    let registered = match record.metadata.get("control_plane_references") {
        None => &[][..],
        Some(Value::Array(references)) => references.as_slice(),
        Some(_) => {
            return Err(ControlPlaneError::invalid_argument(
                "durable control-plane reference registry is not an array",
            ))
        }
    };
    if registered.len() > REGISTERED_REFERENCE_BOUND * 3 {
        return Err(ControlPlaneError::invalid_argument(
            "durable control-plane reference registry exceeds its bound",
        ));
    }
    let mut registered_counts = [0usize; 3];
    for value in registered {
        let stored_type = stored_reference_type(value)?;
        let count = &mut registered_counts[reference_type_index(stored_type)];
        *count += 1;
        if *count > REGISTERED_REFERENCE_BOUND {
            return Err(ControlPlaneError::invalid_argument(format!(
                "durable control-plane {} reference registry exceeds its bound",
                reference_type_name(stored_type)
            )));
        }
        let reference = project_registered_reference(&run, stored_type, value)?;
        if stored_type == reference_type {
            references.push(reference);
        }
    }
    references.sort_by(|left, right| left.reference.cmp(&right.reference));
    let mut unique: Vec<ControlPlaneReference> = Vec::with_capacity(references.len());
    for reference in references {
        if let Some(previous) = unique.last() {
            if previous.reference == reference.reference {
                if previous.registered_at.is_none()
                    && reference.registered_at.is_none()
                    && previous.kind == reference.kind
                    && previous.uri == reference.uri
                {
                    continue;
                }
                return Err(ControlPlaneError::invalid_argument(format!(
                    "run {run} contains duplicate {} reference identities",
                    reference_type_name(reference_type)
                )));
            }
        }
        unique.push(reference);
    }
    Ok(unique)
}

fn reference_type_index(reference_type: ControlPlaneReferenceType) -> usize {
    match reference_type {
        ControlPlaneReferenceType::Artifact => 0,
        ControlPlaneReferenceType::Evidence => 1,
        ControlPlaneReferenceType::ExternalReference => 2,
    }
}

fn stored_reference_type(value: &Value) -> Result<ControlPlaneReferenceType, ControlPlaneError> {
    match value["reference_type"].as_str() {
        Some("artifact") => Ok(ControlPlaneReferenceType::Artifact),
        Some("evidence") => Ok(ControlPlaneReferenceType::Evidence),
        Some("external_reference") => Ok(ControlPlaneReferenceType::ExternalReference),
        _ => Err(ControlPlaneError::invalid_argument(
            "registered reference type is missing or unsupported",
        )),
    }
}

fn project_registered_reference(
    run: &RunId,
    reference_type: ControlPlaneReferenceType,
    value: &Value,
) -> Result<ControlPlaneReference, ControlPlaneError> {
    let string = |name: &str, limit: usize| {
        value[name]
            .as_str()
            .filter(|value| !value.trim().is_empty() && value.len() <= limit)
            .map(str::to_string)
            .ok_or_else(|| {
                ControlPlaneError::invalid_argument(format!(
                    "registered reference {name} is missing"
                ))
            })
    };
    let registered_at = string("registered_at", 128)?;
    if DateTime::parse_from_rfc3339(&registered_at).is_err() {
        return Err(ControlPlaneError::invalid_argument(
            "registered reference timestamp is invalid",
        ));
    }
    let idempotency_digest = string("idempotency_digest", 64)?;
    if idempotency_digest.len() != 64
        || !idempotency_digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(ControlPlaneError::invalid_argument(
            "registered reference idempotency digest is invalid",
        ));
    }
    Ok(ControlPlaneReference {
        schema: CONTROL_PLANE_REFERENCE_SCHEMA.to_string(),
        run: run.clone(),
        reference_type,
        reference: public_reference_id(&string("reference", 256)?)?,
        kind: redacted_bounded(&string("kind", 128)?, 128),
        uri: redacted_reference_uri(&string("uri", 2048)?, 2048),
        registered_at: Some(registered_at),
        actor: Some(redacted_bounded(&string("actor", 256)?, 256)),
    })
}

fn public_reference_id(value: &str) -> Result<ReferenceId, ControlPlaneError> {
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
        || homeboy_core::redaction::redact_string(value) != value
    {
        return Err(ControlPlaneError::invalid_argument(
            "reference identity is not safe for public display",
        ));
    }
    ReferenceId::new(value).map_err(|error| ControlPlaneError::invalid_argument(error.to_string()))
}

fn register_reference_in_store(
    store: &AgentTaskLifecycleStore,
    run: &RunId,
    reference_type: ControlPlaneReferenceType,
    request: &ControlPlaneReferenceRegistration,
) -> Result<ControlPlaneReference, ControlPlaneError> {
    public_reference_id(request.reference.as_str())?;
    let type_name = reference_type_name(reference_type);
    let kind = redacted_bounded(&request.kind, 128);
    let uri = redacted_reference_uri(&request.uri, 2048);
    let actor = redacted_bounded(&request.actor, 256);
    let idempotency_digest =
        homeboy_engine_primitives::content_hash::sha256_hex(request.idempotency_key.as_bytes());
    let registered_at = now_timestamp();
    let mut conflict = None;
    let updated = store
        .mutate_record(run.as_str(), |record| {
            let automatic_id_exists = match reference_type {
                ControlPlaneReferenceType::Artifact => artifact_refs(record),
                ControlPlaneReferenceType::Evidence => evidence_refs(record),
                ControlPlaneReferenceType::ExternalReference => Vec::new(),
            }
            .iter()
            .any(|reference| reference.id == request.reference.as_str());
            if automatic_id_exists {
                conflict = Some(ControlPlaneError::invalid_argument(format!(
                    "{} reference identity is already owned by an automatic run reference",
                    type_name
                )));
                return false;
            }
            let stored = record
                .ensure_metadata_object()
                .entry("control_plane_references".to_string())
                .or_insert_with(|| serde_json::json!([]));
            let Some(references) = stored.as_array_mut() else {
                conflict = Some(ControlPlaneError::invalid_argument(
                    "durable control-plane reference registry is not an array",
                ));
                return false;
            };
            if references.len() > REGISTERED_REFERENCE_BOUND * 3 {
                conflict = Some(ControlPlaneError::invalid_argument(
                    "durable control-plane reference registry exceeds its bound",
                ));
                return false;
            }
            let mut registered_counts = [0usize; 3];
            for existing in references.iter() {
                let validation = stored_reference_type(existing).and_then(|stored_type| {
                    let count = &mut registered_counts[reference_type_index(stored_type)];
                    *count += 1;
                    if *count > REGISTERED_REFERENCE_BOUND {
                        return Err(ControlPlaneError::invalid_argument(format!(
                            "durable control-plane {} reference registry exceeds its bound",
                            reference_type_name(stored_type)
                        )));
                    }
                    project_registered_reference(run, stored_type, existing).map(|_| ())
                });
                if let Err(error) = validation {
                    conflict = Some(error);
                    return false;
                }
            }
            if let Some(existing) = references.iter().find(|existing| {
                existing["idempotency_digest"].as_str() == Some(idempotency_digest.as_str())
            }) {
                let matches = existing["reference_type"].as_str() == Some(type_name)
                    && existing["reference"].as_str() == Some(request.reference.as_str())
                    && existing["kind"].as_str() == Some(kind.as_str())
                    && existing["uri"].as_str() == Some(uri.as_str())
                    && existing["actor"].as_str() == Some(actor.as_str());
                if !matches {
                    conflict = Some(ControlPlaneError::invalid_argument(
                        "reference idempotency key was already used for different inputs",
                    ));
                }
                return false;
            }
            if references.iter().any(|existing| {
                existing["reference_type"].as_str() == Some(type_name)
                    && existing["reference"].as_str() == Some(request.reference.as_str())
            }) {
                conflict = Some(ControlPlaneError::invalid_argument(format!(
                    "{type_name} reference identity is already registered with another idempotency key"
                )));
                return false;
            }
            if references
                .iter()
                .filter(|existing| {
                    existing["reference_type"].as_str() == Some(type_name)
                })
                .count()
                >= REGISTERED_REFERENCE_BOUND
            {
                conflict = Some(ControlPlaneError::invalid_argument(format!(
                    "run has reached the {REGISTERED_REFERENCE_BOUND} registered {type_name} reference limit"
                )));
                return false;
            }
            references.push(serde_json::json!({
                "reference_type": type_name,
                "reference": request.reference,
                "kind": kind,
                "uri": uri,
                "registered_at": registered_at,
                "actor": actor,
                "idempotency_digest": idempotency_digest,
            }));
            true
        })
        .map_err(map_lifecycle_error)?;
    if let Some(error) = conflict {
        return Err(error);
    }
    let record = match updated {
        Some(record) => record,
        None => store
            .read_record(run.as_str())
            .map_err(map_lifecycle_error)?,
    };
    references_for_record(&record, reference_type)?
        .into_iter()
        .find(|reference| reference.reference == request.reference)
        .ok_or_else(|| ControlPlaneError::unavailable("registered reference was not persisted"))
}

fn provider_attempts(
    record: &AgentTaskRunRecord,
    task: &TaskId,
) -> Result<Vec<ControlPlaneAttempt>, ControlPlaneError> {
    let run = RunId::new(&record.run_id)
        .map_err(|error| ControlPlaneError::invalid_argument(error.to_string()))?;
    let task_count = record
        .tasks
        .iter()
        .filter(|candidate| candidate.task_id == task.as_str())
        .count();
    if task_count == 0 {
        return Err(ControlPlaneError::not_found(format!(
            "task not found in run {run}: {task}"
        )));
    }
    if task_count > 1 {
        return Err(ControlPlaneError::invalid_argument(format!(
            "run {run} contains duplicate task identity {task}"
        )));
    }
    let executions = record
        .metadata
        .get("provider_executions")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut attempts = executions
        .iter()
        .filter(|execution| execution["task_id"].as_str() == Some(task.as_str()))
        .map(|execution| project_provider_attempt(&run, task, execution))
        .collect::<Result<Vec<_>, _>>()?;
    attempts.sort_by_key(|attempt| attempt.attempt_number);
    if attempts
        .windows(2)
        .any(|pair| pair[0].attempt_number == pair[1].attempt_number)
    {
        return Err(ControlPlaneError::invalid_argument(format!(
            "run {run}, task {task} contains duplicate provider attempt numbers"
        )));
    }
    Ok(attempts)
}

fn project_provider_attempt(
    run: &RunId,
    task: &TaskId,
    execution: &Value,
) -> Result<ControlPlaneAttempt, ControlPlaneError> {
    let number = execution["attempt"]
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| ControlPlaneError::invalid_argument("provider attempt number is invalid"))?;
    let expected_id = format!("{run}:{task}:{number}");
    let id = execution["owner_identity"].as_str().ok_or_else(|| {
        ControlPlaneError::invalid_argument("provider attempt owner identity is missing")
    })?;
    if id != expected_id {
        return Err(ControlPlaneError::invalid_argument(
            "provider attempt owner identity does not match its run, task, and number",
        ));
    }
    let execution_id = execution
        .get("execution_identity")
        .and_then(Value::as_str)
        .map(|execution_id| {
            if execution_id != format!("{id}:execution") {
                return Err(ControlPlaneError::invalid_argument(
                    "provider execution identity does not match its attempt",
                ));
            }
            ExecutionId::new(execution_id)
                .map_err(|error| ControlPlaneError::invalid_argument(error.to_string()))
        })
        .transpose()?;
    let state = execution["state"]
        .as_str()
        .and_then(provider_attempt_state)
        .ok_or_else(|| ControlPlaneError::invalid_argument("provider attempt state is invalid"))?;
    let started_at = execution["started_at"]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ControlPlaneError::invalid_argument("provider attempt start is missing"))?;
    let finished_at = execution["finished_at"]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    if state.is_terminal() != finished_at.is_some() {
        return Err(ControlPlaneError::invalid_argument(
            "provider attempt terminal state and finish timestamp disagree",
        ));
    }
    Ok(ControlPlaneAttempt {
        schema: CONTROL_PLANE_ATTEMPT_SCHEMA.to_string(),
        run: run.clone(),
        task: task.clone(),
        attempt: AttemptId::new(id)
            .map_err(|error| ControlPlaneError::invalid_argument(error.to_string()))?,
        attempt_number: number,
        state,
        started_at: started_at.to_string(),
        finished_at,
        execution: execution_id,
    })
}

fn provider_attempt_state(state: &str) -> Option<ControlPlaneState> {
    match state {
        "running" => Some(ControlPlaneState::Running),
        "succeeded" => Some(ControlPlaneState::Succeeded),
        "candidate_recoverable" => Some(ControlPlaneState::CandidateRecoverable),
        "failed" => Some(ControlPlaneState::Failed),
        "cancelled" => Some(ControlPlaneState::Cancelled),
        "timed_out" => Some(ControlPlaneState::TimedOut),
        _ => None,
    }
}

fn encode_attempt_cursor(
    run: &RunId,
    task: &TaskId,
    attempt_number: u32,
) -> Result<AttemptCursor, ControlPlaneError> {
    let bytes = serde_json::to_vec(&AttemptCursorPayload {
        schema: ATTEMPT_CURSOR_SCHEMA.to_string(),
        run_id: run.as_str().to_string(),
        task_id: task.as_str().to_string(),
        attempt_number,
    })
    .map_err(|error| ControlPlaneError::unavailable(error.to_string()))?;
    AttemptCursor::new(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
        .map_err(|error| ControlPlaneError::invalid_argument(error.to_string()))
}

fn decode_attempt_cursor(
    cursor: &AttemptCursor,
    run: &RunId,
    task: &TaskId,
) -> Result<u32, ControlPlaneError> {
    if cursor.as_str().len() > RUN_CURSOR_BOUND {
        return Err(ControlPlaneError::invalid_argument(
            "control-plane attempt cursor exceeds the size bound",
        ));
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cursor.as_str())
        .map_err(|_| {
            ControlPlaneError::invalid_argument("control-plane attempt cursor is invalid")
        })?;
    let payload: AttemptCursorPayload = serde_json::from_slice(&bytes).map_err(|_| {
        ControlPlaneError::invalid_argument("control-plane attempt cursor is invalid")
    })?;
    if payload.schema != ATTEMPT_CURSOR_SCHEMA
        || payload.run_id != run.as_str()
        || payload.task_id != task.as_str()
        || payload.attempt_number == 0
    {
        return Err(ControlPlaneError::invalid_argument(
            "control-plane attempt cursor is invalid for this task",
        ));
    }
    Ok(payload.attempt_number)
}

fn project_task(
    record: &AgentTaskRunRecord,
    task: &crate::agent_task_lifecycle::AgentTaskRunTask,
) -> Result<ControlPlaneTask, ControlPlaneError> {
    let run = RunId::new(&record.run_id)
        .map_err(|error| ControlPlaneError::invalid_argument(error.to_string()))?;
    let task_id = TaskId::new(&task.task_id)
        .map_err(|error| ControlPlaneError::invalid_argument(error.to_string()))?;
    let mission = crate::agent_task_lifecycle::canonical_mission(record)
        .map_err(|error| ControlPlaneError::invalid_argument(error.message))?;
    Ok(ControlPlaneTask {
        schema: CONTROL_PLANE_TASK_SCHEMA.to_string(),
        mission,
        run,
        task: task_id,
        state: task_state(task.state),
    })
}

fn task_state(state: crate::agent_tasks::AgentTaskState) -> ControlPlaneState {
    match state {
        crate::agent_tasks::AgentTaskState::Queued => ControlPlaneState::Queued,
        crate::agent_tasks::AgentTaskState::Blocked => ControlPlaneState::Blocked,
        crate::agent_tasks::AgentTaskState::Skipped => ControlPlaneState::Skipped,
        crate::agent_tasks::AgentTaskState::Running => ControlPlaneState::Running,
        crate::agent_tasks::AgentTaskState::Succeeded => ControlPlaneState::Succeeded,
        crate::agent_tasks::AgentTaskState::CandidateRecoverable => {
            ControlPlaneState::CandidateRecoverable
        }
        crate::agent_tasks::AgentTaskState::Failed => ControlPlaneState::Failed,
        crate::agent_tasks::AgentTaskState::Cancelled => ControlPlaneState::Cancelled,
        crate::agent_tasks::AgentTaskState::TimedOut => ControlPlaneState::TimedOut,
    }
}

fn encode_task_cursor(run: &RunId, task: &TaskId) -> Result<TaskCursor, ControlPlaneError> {
    let bytes = serde_json::to_vec(&TaskCursorPayload {
        schema: TASK_CURSOR_SCHEMA.to_string(),
        run_id: run.as_str().to_string(),
        task_id: task.as_str().to_string(),
    })
    .map_err(|error| ControlPlaneError::unavailable(error.to_string()))?;
    TaskCursor::new(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
        .map_err(|error| ControlPlaneError::invalid_argument(error.to_string()))
}

fn decode_task_cursor(cursor: &TaskCursor, run: &RunId) -> Result<String, ControlPlaneError> {
    if cursor.as_str().len() > RUN_CURSOR_BOUND {
        return Err(ControlPlaneError::invalid_argument(
            "control-plane task cursor exceeds the size bound",
        ));
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cursor.as_str())
        .map_err(|_| ControlPlaneError::invalid_argument("control-plane task cursor is invalid"))?;
    let payload: TaskCursorPayload = serde_json::from_slice(&bytes)
        .map_err(|_| ControlPlaneError::invalid_argument("control-plane task cursor is invalid"))?;
    if payload.schema != TASK_CURSOR_SCHEMA
        || payload.run_id != run.as_str()
        || payload.task_id.trim().is_empty()
    {
        return Err(ControlPlaneError::invalid_argument(
            "control-plane task cursor is invalid for this run",
        ));
    }
    Ok(payload.task_id)
}

impl<L: RunLookup + MissionLookup> OrchestrationService<L> {
    pub fn mission(&self, id: &MissionId) -> Result<ControlPlaneMission, ControlPlaneError> {
        self.lookup
            .get_mission(id)?
            .map(project_mission)
            .transpose()?
            .ok_or_else(|| ControlPlaneError::not_found(format!("mission not found: {id}")))
    }

    pub fn missions(
        &self,
        request: &ControlPlaneMissionListRequest,
    ) -> Result<ControlPlaneMissionPage, ControlPlaneError> {
        request.validate()?;
        let after = request
            .cursor
            .as_ref()
            .map(decode_mission_cursor)
            .transpose()?;
        let page = self
            .lookup
            .list_missions(after.as_ref(), request.limit as usize)?;
        let missions = page
            .missions
            .into_iter()
            .map(project_mission)
            .collect::<Result<Vec<_>, _>>()?;
        let next_cursor = page
            .next_cursor
            .as_ref()
            .map(encode_mission_cursor)
            .transpose()?;
        Ok(ControlPlaneMissionPage {
            schema: CONTROL_PLANE_MISSION_PAGE_SCHEMA.to_string(),
            missions,
            has_more: next_cursor.is_some(),
            next_cursor,
        })
    }
}

fn project_mission(
    mission: homeboy_core::observation::MissionRecord,
) -> Result<ControlPlaneMission, ControlPlaneError> {
    Ok(ControlPlaneMission {
        schema: CONTROL_PLANE_MISSION_SCHEMA.to_string(),
        mission: MissionId::new(mission.id)
            .map_err(|error| ControlPlaneError::invalid_argument(error.to_string()))?,
        created_at: mission.created_at,
        updated_at: mission.updated_at,
        run_count: mission.run_count,
    })
}

fn encode_mission_cursor(
    position: &homeboy_core::observation::MissionCursor,
) -> Result<MissionCursor, ControlPlaneError> {
    let bytes = serde_json::to_vec(&MissionCursorPayload {
        schema: MISSION_CURSOR_SCHEMA.to_string(),
        created_at: position.created_at.clone(),
        mission_id: position.id.clone(),
    })
    .map_err(|error| ControlPlaneError::unavailable(error.to_string()))?;
    MissionCursor::new(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
        .map_err(|error| ControlPlaneError::invalid_argument(error.to_string()))
}

fn decode_mission_cursor(
    cursor: &MissionCursor,
) -> Result<homeboy_core::observation::MissionCursor, ControlPlaneError> {
    if cursor.as_str().len() > RUN_CURSOR_BOUND {
        return Err(ControlPlaneError::invalid_argument(
            "control-plane mission cursor exceeds the size bound",
        ));
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cursor.as_str())
        .map_err(|_| {
            ControlPlaneError::invalid_argument("control-plane mission cursor is invalid")
        })?;
    let payload: MissionCursorPayload = serde_json::from_slice(&bytes).map_err(|_| {
        ControlPlaneError::invalid_argument("control-plane mission cursor is invalid")
    })?;
    if payload.schema != MISSION_CURSOR_SCHEMA
        || payload.created_at.trim().is_empty()
        || payload.mission_id.trim().is_empty()
    {
        return Err(ControlPlaneError::invalid_argument(
            "control-plane mission cursor is invalid",
        ));
    }
    Ok(homeboy_core::observation::MissionCursor {
        created_at: payload.created_at,
        id: payload.mission_id,
    })
}

impl<L: RunLookup + RunListLookup> OrchestrationService<L> {
    /// Stable, bounded, non-reconciling discovery over immutable submission
    /// ordering. The cursor names the final run in the preceding page.
    pub fn runs(
        &self,
        request: &ControlPlaneRunListRequest,
    ) -> Result<ControlPlaneRunPage, ControlPlaneError> {
        request.validate()?;
        let decoded = request.cursor.as_ref().map(decode_run_cursor).transpose()?;
        if let Some((_, cursor_mission)) = &decoded {
            if cursor_mission.as_ref() != request.mission.as_ref() {
                return Err(ControlPlaneError::invalid_argument(
                    "control-plane run cursor does not match the mission filter",
                ));
            }
        }
        let after = decoded.as_ref().map(|(position, _)| position);
        let page = self
            .lookup
            .list(request.mission.as_ref(), after, request.limit as usize)?;
        let runs = page
            .snapshots
            .into_iter()
            .map(|snapshot| project_record(&snapshot.record, snapshot.plan.as_ref()))
            .collect::<Result<Vec<_>, _>>()?;
        let has_more = page.next_position.is_some();
        let next_cursor = page
            .next_position
            .as_ref()
            .map(|position| encode_run_cursor(position, request.mission.as_ref()))
            .transpose()?;
        Ok(ControlPlaneRunPage {
            schema: CONTROL_PLANE_RUN_PAGE_SCHEMA.to_string(),
            runs,
            next_cursor,
            has_more,
        })
    }
}

fn encode_run_cursor(
    position: &RunPagePosition,
    mission: Option<&MissionId>,
) -> Result<RunCursor, ControlPlaneError> {
    let bytes = serde_json::to_vec(&RunCursorPayload {
        schema: RUN_CURSOR_SCHEMA.to_string(),
        started_at: position.started_at.clone(),
        run_id: position.run_id.clone(),
        mission_id: mission.map(|mission| mission.as_str().to_string()),
    })
    .map_err(|error| ControlPlaneError::unavailable(error.to_string()))?;
    RunCursor::new(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
        .map_err(|error| ControlPlaneError::invalid_argument(error.to_string()))
}

fn decode_run_cursor(
    cursor: &RunCursor,
) -> Result<(RunPagePosition, Option<MissionId>), ControlPlaneError> {
    if cursor.as_str().len() > RUN_CURSOR_BOUND {
        return Err(ControlPlaneError::invalid_argument(
            "control-plane run cursor exceeds the size bound",
        ));
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cursor.as_str())
        .map_err(|_| ControlPlaneError::invalid_argument("control-plane run cursor is invalid"))?;
    let payload: RunCursorPayload = serde_json::from_slice(&bytes)
        .map_err(|_| ControlPlaneError::invalid_argument("control-plane run cursor is invalid"))?;
    if payload.schema != RUN_CURSOR_SCHEMA
        || payload.started_at.trim().is_empty()
        || payload.run_id.trim().is_empty()
    {
        return Err(ControlPlaneError::invalid_argument(
            "control-plane run cursor is invalid",
        ));
    }
    let mission = payload
        .mission_id
        .map(MissionId::new)
        .transpose()
        .map_err(|_| ControlPlaneError::invalid_argument("control-plane run cursor is invalid"))?;
    Ok((
        RunPagePosition {
            started_at: payload.started_at,
            run_id: payload.run_id,
        },
        mission,
    ))
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
                        &crate::agent_task_promotion::AgentTaskPromotionRequest {
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
                let acknowledgement = serde_json::from_value(result).map_err(|error| {
                    ControlPlaneError::unavailable(format!("stored action result: {error}"))
                })?;
                ensure_action_events_in_store(
                    &self.lookup.store,
                    &record,
                    request,
                    &operation_key,
                    &acknowledgement,
                )?;
                Ok(acknowledgement)
            }
            ClaimOutcome::LeaseHeld => {
                if operation_lease_is_active_in_store(&self.lookup.store, &resolved, &operation_key)
                    .map_err(map_lifecycle_error)?
                {
                    return Err(ControlPlaneError::unavailable(
                        "this idempotent action is already in progress",
                    ));
                }
                let recovered = recover_interrupted_action_acknowledgement(
                    &self.lookup.store,
                    &record,
                    request,
                    &operation_key,
                )?;
                complete_cook_operation_in_store(
                    &self.lookup.store,
                    &resolved,
                    &operation_key,
                    serde_json::to_value(&recovered).map_err(|error| {
                        ControlPlaneError::unavailable(format!(
                            "serialize recovered action result: {error}"
                        ))
                    })?,
                )
                .map_err(map_lifecycle_error)?;
                let stored =
                    operation_claim_in_store(&self.lookup.store, &resolved, &operation_key)
                        .map_err(map_lifecycle_error)?
                        .and_then(|claim| claim.result)
                        .ok_or_else(|| {
                            ControlPlaneError::unavailable(
                                "recovered action acknowledgement was not persisted",
                            )
                        })?;
                let acknowledgement = serde_json::from_value(stored).map_err(|error| {
                    ControlPlaneError::unavailable(format!(
                        "stored recovered action result: {error}"
                    ))
                })?;
                ensure_action_events_in_store(
                    &self.lookup.store,
                    &record,
                    request,
                    &operation_key,
                    &acknowledgement,
                )?;
                Ok(acknowledgement)
            }
            ClaimOutcome::Acquired => {
                let accepted_at =
                    operation_claim_in_store(&self.lookup.store, &resolved, &operation_key)
                        .map_err(map_lifecycle_error)?
                        .and_then(|claim| claim.accepted_at)
                        .ok_or_else(|| {
                            ControlPlaneError::unavailable(
                                "action claim has no durable acceptance timestamp",
                            )
                        })?;
                let acknowledgement = format!(
                    "{}:action:{}:{}",
                    record.run_id,
                    action_name(request.action),
                    request.idempotency_key
                );
                append_action_event_in_store(
                    &self.lookup.store,
                    &record,
                    request,
                    &operation_key,
                    "action.accepted",
                    &accepted_at,
                    serde_json::json!({
                        "operation_digest": action_operation_digest(&operation_key),
                        "action": request.action,
                        "acknowledgement": acknowledgement,
                        "actor": request.actor,
                        "expected_updated_at": request.expected_updated_at,
                        "confirmed": request.confirmed,
                        "parameters": request.parameters,
                    }),
                )?;
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
                        ControlPlaneAction::PlacementUpdate => {
                            let parameters: ControlPlanePlacementUpdateParameters =
                                serde_json::from_value(request.parameters.data.clone()).map_err(
                                    |error| {
                                        ControlPlaneError::invalid_argument(format!(
                                            "placement update parameters: {error}"
                                        ))
                                    },
                                )?;
                            let updated = (|| -> homeboy_core::Result<_> {
                                let updated = crate::agent_task_lifecycle::update_unmaterialized_cook_placement_in_store(
                                    &self.lookup.store,
                                    &resolved,
                                    &parameters.placement,
                                    &request.actor,
                                )?;
                                let reconciliation = crate::agent_task_service::reconcile_unmaterialized_cook_admission(
                                    &resolved,
                                )?;
                                let current = self.lookup.store.read_record(&resolved)?;
                                Ok((updated, current, reconciliation))
                            })();
                            match updated {
                                Ok((_updated, current, reconciliation)) => (
                                    ControlPlaneActionOutcome::Succeeded,
                                    project_record(&current, None)?,
                                    ControlPlaneActionPayload {
                                        schema: CONTROL_PLANE_PLACEMENT_UPDATE_RESULT_SCHEMA
                                            .to_string(),
                                        data: serde_json::json!({
                                            "schema": CONTROL_PLANE_PLACEMENT_UPDATE_RESULT_SCHEMA,
                                            "run_id": current.run_id,
                                            "placement": parameters.placement,
                                            "reconciliation": reconciliation,
                                            "preserved_identity": true,
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
                        ControlPlaneAction::Retry => {
                            let mut parameters: ControlPlaneRetryParameters =
                                serde_json::from_value(request.parameters.data.clone()).map_err(
                                    |error| {
                                        ControlPlaneError::invalid_argument(format!(
                                            "retry parameters: {error}"
                                        ))
                                    },
                                )?;
                            parameters.new_run_id =
                                Some(retry_action_run_id(&record, request, &parameters));
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
                        ControlPlaneAction::Quarantine => {
                            let parameters: ControlPlaneQuarantineParameters =
                                serde_json::from_value(request.parameters.data.clone()).map_err(
                                    |error| {
                                        ControlPlaneError::invalid_argument(format!(
                                            "quarantine parameters: {error}"
                                        ))
                                    },
                                )?;
                            match crate::agent_task_lifecycle::quarantine_queued_run_exact_in_store(
                                &self.lookup.store,
                                &resolved,
                                &parameters.reason,
                            ) {
                                Ok(current) => (
                                    ControlPlaneActionOutcome::Succeeded,
                                    project_record(&current, None)?,
                                    ControlPlaneActionPayload {
                                        schema: CONTROL_PLANE_QUARANTINE_RESULT_SCHEMA.to_string(),
                                        data: serde_json::json!({ "record": current }),
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
                        ControlPlaneAction::Rearm => {
                            match crate::agent_task_lifecycle::rearm_quarantined_run_in_store(
                                &self.lookup.store,
                                &resolved,
                            ) {
                                Ok(current) => (
                                    ControlPlaneActionOutcome::Succeeded,
                                    project_record(&current, None)?,
                                    ControlPlaneActionPayload {
                                        schema: CONTROL_PLANE_REARM_RESULT_SCHEMA.to_string(),
                                        data: serde_json::json!({ "record": current }),
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
                ensure_action_events_in_store(
                    &self.lookup.store,
                    &record,
                    request,
                    &operation_key,
                    &result,
                )?;
                Ok(result)
            }
        }
    }
}

fn retry_action_run_id(
    record: &AgentTaskRunRecord,
    request: &ControlPlaneActionRequest,
    parameters: &ControlPlaneRetryParameters,
) -> String {
    parameters.new_run_id.clone().unwrap_or_else(|| {
        let identity = uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_OID,
            format!("{}:retry:{}", record.run_id, request.idempotency_key).as_bytes(),
        );
        format!("retry-{identity}")
    })
}

fn recover_interrupted_action_acknowledgement(
    store: &AgentTaskLifecycleStore,
    original: &AgentTaskRunRecord,
    request: &ControlPlaneActionRequest,
    operation_key: &str,
) -> Result<ControlPlaneActionAcknowledgement, ControlPlaneError> {
    let current = store
        .read_record(&original.run_id)
        .map_err(map_lifecycle_error)?;
    let claim = operation_claim_in_store(store, &original.run_id, operation_key)
        .map_err(map_lifecycle_error)?
        .ok_or_else(|| ControlPlaneError::unavailable("interrupted action claim is missing"))?;
    let accepted_at = claim
        .accepted_at
        .or_else(|| {
            action_accepted_at(store, &original.run_id, operation_key)
                .ok()
                .flatten()
        })
        .unwrap_or(claim.leased_at);
    let failed = |message: &str| {
        Ok((
            ControlPlaneActionOutcome::Failed,
            project_record(&current, None)?,
            ControlPlaneActionPayload::empty(),
            Some(message.to_string()),
        ))
    };
    let (outcome, resource, result, message) = match request.action {
        ControlPlaneAction::Cancel if current.state.is_terminal() => (
            if current.state == AgentTaskRunState::Cancelled {
                ControlPlaneActionOutcome::Succeeded
            } else {
                ControlPlaneActionOutcome::AlreadySatisfied
            },
            project_record(&current, None)?,
            cancel_result_payload(cancel_result_for_record(
                &current,
                Duration::ZERO,
                0,
                None,
            )),
            Some("recovered from the durable terminal run state".to_string()),
        ),
        ControlPlaneAction::Cancel => failed(
            "cancel was interrupted after acceptance and its external outcome is ambiguous; reconcile the run before issuing a new action",
        )?,
        ControlPlaneAction::Reconcile => failed(
            "reconcile was interrupted after acceptance and its external outcome is ambiguous; inspect current state before issuing a new action",
        )?,
        ControlPlaneAction::PlacementUpdate => failed(
            "placement update was interrupted after acceptance without authoritative completion evidence; no second route change was attempted",
        )?,
        ControlPlaneAction::Retry => {
            let parameters: ControlPlaneRetryParameters =
                serde_json::from_value(request.parameters.data.clone()).map_err(|error| {
                    ControlPlaneError::invalid_argument(format!("retry parameters: {error}"))
                })?;
            let retry_run_id = retry_action_run_id(original, request, &parameters);
            if store
                .record_exists(&retry_run_id)
                .map_err(map_lifecycle_error)?
            {
                let successor = store
                    .read_record(&retry_run_id)
                    .map_err(map_lifecycle_error)?;
                if successor.metadata["retry_of"] == original.run_id {
                    (
                        ControlPlaneActionOutcome::AlreadySatisfied,
                        project_record(&successor, None)?,
                        ControlPlaneActionPayload {
                            schema: CONTROL_PLANE_RETRY_RESULT_SCHEMA.to_string(),
                            data: serde_json::json!({
                                "record": successor,
                                "runnable": false,
                                "created": false,
                                "recovered": true,
                            }),
                        },
                        Some("recovered from the durable retry successor".to_string()),
                    )
                } else {
                    failed(
                        "retry was interrupted and the deterministic successor identity belongs to different lineage",
                    )?
                }
            } else {
                failed(
                    "retry was interrupted after acceptance without an authoritative durable successor; no second dispatch was attempted",
                )?
            }
        }
        ControlPlaneAction::Quarantine if current.metadata.get("queue_quarantine").is_some() => (
            ControlPlaneActionOutcome::Succeeded,
            project_record(&current, None)?,
            ControlPlaneActionPayload {
                schema: CONTROL_PLANE_QUARANTINE_RESULT_SCHEMA.to_string(),
                data: serde_json::json!({ "record": current, "recovered": true }),
            },
            Some("recovered from the durable quarantine marker".to_string()),
        ),
        ControlPlaneAction::Quarantine => failed(
            "quarantine was interrupted after acceptance without a durable quarantine marker; no second mutation was attempted",
        )?,
        ControlPlaneAction::Rearm if current.metadata.get("queue_quarantine").is_none() => (
            ControlPlaneActionOutcome::Succeeded,
            project_record(&current, None)?,
            ControlPlaneActionPayload {
                schema: CONTROL_PLANE_REARM_RESULT_SCHEMA.to_string(),
                data: serde_json::json!({ "record": current, "recovered": true }),
            },
            Some("recovered from the durable rearm state".to_string()),
        ),
        ControlPlaneAction::Rearm => failed(
            "rearm was interrupted after acceptance without authoritative completion evidence; no second mutation was attempted",
        )?,
        ControlPlaneAction::Resume
            if current
                .metadata
                .get("unmaterialized_cook_admission")
                .is_some_and(Value::is_object)
                && current.state.is_terminal() =>
        {
            (
                ControlPlaneActionOutcome::AlreadySatisfied,
                project_record(&current, None)?,
                ControlPlaneActionPayload {
                    schema: "homeboy/unmaterialized-cook-resume/v1".to_string(),
                    data: serde_json::json!({
                        "schema": "homeboy/unmaterialized-cook-resume/v1",
                        "status": current.metadata["unmaterialized_cook_admission"]["state"],
                        "run_id": current.run_id,
                        "idempotent": true,
                        "terminal": true,
                        "terminal_state": current.state,
                        "recovered": true,
                    }),
                },
                Some("recovered from the durable terminal admission state".to_string()),
            )
        }
        ControlPlaneAction::Resume if current.state.is_terminal() => {
            match crate::agent_task_lifecycle::read_aggregate_in_store(store, &current.run_id) {
                Ok(aggregate) => {
                    let exit_code = crate::agent_task_service::aggregate_exit_code(&aggregate);
                    let aggregate =
                        crate::agent_task_artifacts::reviewer_facing_aggregate(&aggregate);
                    (
                        ControlPlaneActionOutcome::AlreadySatisfied,
                        project_record(&current, None)?,
                        ControlPlaneActionPayload {
                            schema: CONTROL_PLANE_RESUME_RESULT_SCHEMA.to_string(),
                            data: serde_json::json!({
                                "aggregate": aggregate,
                                "exit_code": exit_code,
                                "recovered": true,
                            }),
                        },
                        Some("recovered from the durable terminal aggregate".to_string()),
                    )
                }
                Err(_) => failed(
                    "resume was interrupted and the terminal run has no authoritative aggregate; no second execution was attempted",
                )?,
            }
        }
        ControlPlaneAction::Resume => failed(
            "resume was interrupted after acceptance without authoritative terminal evidence; no second execution was attempted",
        )?,
        ControlPlaneAction::Promote => {
            let parameters: crate::agent_task_service::AgentTaskPromotionRequest =
                serde_json::from_value(request.parameters.data.clone()).map_err(|error| {
                    ControlPlaneError::invalid_argument(format!("promote parameters: {error}"))
                })?;
            let request_fingerprint =
                crate::agent_task_service::promotion_request_fingerprint(&parameters)
                    .map_err(map_lifecycle_error)?;
            let report = current
                .metadata
                .get("latest_promotion")
                .cloned()
                .and_then(|value| serde_json::from_value(value).ok())
                .filter(
                    |report: &crate::agent_task_promotion::AgentTaskPromotionReport| {
                        report.source.run_id.as_deref() == Some(original.run_id.as_str())
                            && report.to_worktree == parameters.to_worktree
                            && parameters
                                .task_id
                                .as_deref()
                                .is_none_or(|task| report.source.task_id == task)
                            && parameters
                                .artifact_id
                                .as_deref()
                                .is_none_or(|artifact| report.patch_artifact.id == artifact)
                            && report.provenance["promotion_request_fingerprint"]
                                == request_fingerprint
                            && report.status
                                != crate::agent_task_promotion::AgentTaskPromotionStatus::VerificationPending
                    },
                );
            if let Some(report) = report {
                let mut data = serde_json::to_value(&report).unwrap_or_default();
                data["handoff"] = promotion_handoff(&report);
                data["recorded_on_run"] = serde_json::json!({
                    "run_id": current.run_id,
                    "metadata_key": "latest_promotion",
                });
                (
                    ControlPlaneActionOutcome::Succeeded,
                    project_record(&current, None)?,
                    ControlPlaneActionPayload {
                        schema: CONTROL_PLANE_PROMOTE_RESULT_SCHEMA.to_string(),
                        data,
                    },
                    Some("recovered from the durable promotion report".to_string()),
                )
            } else {
                failed(
                    "promote was interrupted without a matching terminal promotion report; no second mutation was attempted",
                )?
            }
        }
    };

    Ok(ControlPlaneActionAcknowledgement {
        schema: CONTROL_PLANE_ACTION_ACKNOWLEDGEMENT_SCHEMA.to_string(),
        acknowledgement: format!(
            "{}:action:{}:{}",
            original.run_id,
            action_name(request.action),
            request.idempotency_key
        ),
        run: RunId::new(&original.run_id)
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
    })
}

/// Action receipts live in migration 19, separate from the lifecycle record
/// that owns effect claims. The accepted receipt is committed before the effect;
/// the terminal receipt is retried from the immutable acknowledgement. A crash
/// after an external effect but before `complete_cook_operation_in_store` still
/// has the operation-claim recovery semantics documented by `operation_claims`;
/// filesystem projections and SQLite cannot form one atomic transaction.
fn ensure_action_events_in_store(
    store: &AgentTaskLifecycleStore,
    record: &AgentTaskRunRecord,
    request: &ControlPlaneActionRequest,
    operation_key: &str,
    acknowledgement: &ControlPlaneActionAcknowledgement,
) -> Result<(), ControlPlaneError> {
    if !action_event_receipt_exists(store, record, operation_key, "action.accepted")? {
        append_action_event_in_store(
            store,
            record,
            request,
            operation_key,
            "action.accepted",
            &acknowledgement.accepted_at,
            serde_json::json!({
                "operation_digest": action_operation_digest(operation_key),
                "action": acknowledgement.action,
                "acknowledgement": acknowledgement.acknowledgement,
                "actor": acknowledgement.actor,
                "expected_updated_at": request.expected_updated_at,
                "confirmed": request.confirmed,
                "parameters": request.parameters,
            }),
        )?;
    }
    let kind = match acknowledgement.outcome {
        ControlPlaneActionOutcome::Succeeded => "action.succeeded",
        ControlPlaneActionOutcome::AlreadySatisfied => "action.already_satisfied",
        ControlPlaneActionOutcome::Failed => "action.failed",
    };
    if !action_event_receipt_exists(store, record, operation_key, kind)? {
        append_action_event_in_store(
            store,
            record,
            request,
            operation_key,
            kind,
            &acknowledgement.completed_at,
            serde_json::json!({
                "operation_digest": action_operation_digest(operation_key),
                "action": acknowledgement.action,
                "acknowledgement": acknowledgement.acknowledgement,
                "actor": acknowledgement.actor,
                "outcome": acknowledgement.outcome,
                "resource_state": acknowledgement.resource.state,
                "resource_updated_at": acknowledgement.resource.updated_at,
                "message": acknowledgement.message,
            }),
        )?;
    }
    Ok(())
}

fn action_event_receipt_exists(
    store: &AgentTaskLifecycleStore,
    record: &AgentTaskRunRecord,
    operation_key: &str,
    kind: &str,
) -> Result<bool, ControlPlaneError> {
    let run = RunId::new(&record.run_id)
        .map_err(|error| ControlPlaneError::invalid_argument(error.to_string()))?;
    let digest = homeboy_engine_primitives::content_hash::sha256_hex(
        action_event_idempotency_key(operation_key, kind).as_bytes(),
    );
    store
        .open_observation_initialized()
        .and_then(|observation| observation.control_plane_event_receipt_exists(&run, &digest))
        .map_err(map_lifecycle_error)
}

fn action_accepted_at(
    store: &AgentTaskLifecycleStore,
    run_id: &str,
    operation_key: &str,
) -> Result<Option<String>, ControlPlaneError> {
    let run = RunId::new(run_id)
        .map_err(|error| ControlPlaneError::invalid_argument(error.to_string()))?;
    let operation_digest = action_operation_digest(operation_key);
    store
        .open_observation_initialized()
        .and_then(|observation| observation.control_plane_event_stream(&run))
        .map(|events| {
            events.and_then(|events| {
                events
                    .into_iter()
                    .find(|event| {
                        event.kind == "action.accepted"
                            && event.data["operation_digest"] == operation_digest
                    })
                    .and_then(|event| event.occurred_at)
            })
        })
        .map_err(map_lifecycle_error)
}

fn append_action_event_in_store(
    store: &AgentTaskLifecycleStore,
    record: &AgentTaskRunRecord,
    request: &ControlPlaneActionRequest,
    operation_key: &str,
    kind: &str,
    occurred_at: &str,
    data: Value,
) -> Result<(), ControlPlaneError> {
    append_event_in_store(
        store,
        &RunId::new(&record.run_id)
            .map_err(|error| ControlPlaneError::invalid_argument(error.to_string()))?,
        &ControlPlaneEventAppendRequest {
            schema: CONTROL_PLANE_EVENT_APPEND_REQUEST_SCHEMA.to_string(),
            idempotency_key: action_event_idempotency_key(operation_key, kind),
            actor: request.actor.clone(),
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
        },
    )
    .map(|_| ())
}

pub(crate) fn action_event_idempotency_key(operation_key: &str, kind: &str) -> String {
    format!(
        "{INTERNAL_ACTION_EVENT_KEY_PREFIX}{kind}:{}",
        action_operation_digest(operation_key)
    )
}

fn validate_external_event_append_request(
    request: &ControlPlaneEventAppendRequest,
) -> Result<(), ControlPlaneError> {
    if request
        .idempotency_key
        .starts_with(INTERNAL_ACTION_EVENT_KEY_PREFIX)
        || request.kind.starts_with("action.")
        || request.source.component == "control-plane"
    {
        return Err(ControlPlaneError::invalid_argument(
            "control-plane action event identities are reserved for the controller",
        ));
    }
    Ok(())
}

fn action_operation_digest(operation_key: &str) -> String {
    homeboy_engine_primitives::content_hash::sha256_hex(operation_key.as_bytes())
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
    request.validate()?;
    if request.action == ControlPlaneAction::Promote {
        serde_json::from_value::<crate::agent_task_service::AgentTaskPromotionRequest>(
            request.parameters.data.clone(),
        )
        .map_err(|error| {
            ControlPlaneError::invalid_argument(format!("promote parameters: {error}"))
        })?;
    }
    Ok(())
}

const fn action_name(action: ControlPlaneAction) -> &'static str {
    match action {
        ControlPlaneAction::Cancel => "cancel",
        ControlPlaneAction::Resume => "resume",
        ControlPlaneAction::PlacementUpdate => "placement_update",
        ControlPlaneAction::Retry => "retry",
        ControlPlaneAction::Quarantine => "quarantine",
        ControlPlaneAction::Rearm => "rearm",
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

fn append_event_in_store(
    store: &AgentTaskLifecycleStore,
    run: &RunId,
    request: &ControlPlaneEventAppendRequest,
) -> Result<homeboy_control_plane_contract::ControlPlaneEvent, ControlPlaneError> {
    request.validate()?;
    let mut request = request.clone();
    request.actor = redacted_bounded(&request.actor, 256);
    request.kind = redacted_bounded(&request.kind, 128);
    request.source.component = redacted_bounded(&request.source.component, 128);
    request.source.instance = request
        .source
        .instance
        .as_deref()
        .and_then(|value| nonempty_redacted_bounded(value, 256));
    normalize_event_references(&mut request.artifacts)?;
    normalize_event_references(&mut request.evidence)?;
    request.data = homeboy_core::redaction::redact_json(&request.data);
    store
        .with_config_lock(|| {
            let record = store.read_record(run.as_str())?;
            validate_event_scope(&record, &request).map_err(|error| {
                homeboy_core::Error::validation_invalid_argument(
                    "event_scope",
                    error.message,
                    None,
                    None,
                )
            })?;
            let idempotency_digest = homeboy_engine_primitives::content_hash::sha256_hex(
                request.idempotency_key.as_bytes(),
            );
            let request_digest =
                homeboy_engine_primitives::content_hash::sha256_hex(&serde_json::to_vec(&request)?);
            store
                .open_observation_initialized()?
                .append_control_plane_event(run, &request, &idempotency_digest, &request_digest)
        })
        .map_err(map_lifecycle_error)
}

fn normalize_event_references(
    references: &mut [ControlPlaneEvidenceRef],
) -> Result<(), ControlPlaneError> {
    for reference in references {
        reference.id = redacted_bounded(&reference.id, ID_BOUND);
        reference.kind = redacted_bounded(&reference.kind, STATE_BOUND);
        reference.uri = redacted_reference_uri(&reference.uri, URI_BOUND);
        if reference.id.trim().is_empty()
            || reference.kind.trim().is_empty()
            || reference.uri.trim().is_empty()
        {
            return Err(ControlPlaneError::invalid_argument(
                "control-plane event reference fields must remain nonempty after redaction",
            ));
        }
    }
    Ok(())
}

fn validate_event_scope(
    record: &AgentTaskRunRecord,
    request: &ControlPlaneEventAppendRequest,
) -> Result<(), ControlPlaneError> {
    let Some(task) = request.task.as_ref() else {
        if request.attempt.is_some() || request.execution.is_some() {
            return Err(ControlPlaneError::invalid_argument(
                "control-plane event attempt and execution identities require a task identity",
            ));
        }
        return Ok(());
    };
    let task_count = record
        .tasks
        .iter()
        .filter(|candidate| candidate.task_id == task.as_str())
        .count();
    if task_count != 1 {
        return Err(ControlPlaneError::invalid_argument(format!(
            "control-plane event task does not uniquely belong to run {}",
            record.run_id
        )));
    }
    let Some(attempt_id) = request.attempt.as_ref() else {
        if request.execution.is_some() {
            return Err(ControlPlaneError::invalid_argument(
                "control-plane event execution identity requires an attempt identity",
            ));
        }
        return Ok(());
    };
    let attempts = provider_attempts(record, task)?;
    let attempt = attempts
        .iter()
        .find(|candidate| candidate.attempt == *attempt_id)
        .ok_or_else(|| {
            ControlPlaneError::invalid_argument(format!(
                "control-plane event attempt does not belong to run {} and task {task}",
                record.run_id
            ))
        })?;
    if request
        .execution
        .as_ref()
        .is_some_and(|execution| attempt.execution.as_ref() != Some(execution))
    {
        return Err(ControlPlaneError::invalid_argument(
            "control-plane event execution does not belong to its run, task, and attempt",
        ));
    }
    Ok(())
}

impl<L: EventLookup> OrchestrationService<L> {
    pub fn events(
        &self,
        requested_id: &RunId,
        cursor: Option<&homeboy_control_plane_contract::EventCursor>,
    ) -> Result<homeboy_control_plane_contract::ControlPlaneEventPage, ControlPlaneError> {
        self.lookup.events(requested_id, cursor)?.ok_or_else(|| {
            ControlPlaneError::not_found(format!("agent-task run not found: {requested_id}"))
        })
    }

    pub fn event_retention(
        &self,
        requested_id: &RunId,
    ) -> Result<ControlPlaneEventRetention, ControlPlaneError> {
        self.lookup.event_retention(requested_id)?.ok_or_else(|| {
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
            ControlPlaneErrorClass::NotFound
            | ControlPlaneErrorClass::InvalidArgument
            | ControlPlaneErrorClass::CursorExpired => {
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
            ControlPlaneErrorClass::Unknown => {
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
            ControlPlaneErrorClass::NotFound
            | ControlPlaneErrorClass::InvalidArgument
            | ControlPlaneErrorClass::CursorExpired => {
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
            ControlPlaneErrorClass::Unknown => {
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
                false,
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
            ControlPlaneErrorClass::NotFound
            | ControlPlaneErrorClass::InvalidArgument
            | ControlPlaneErrorClass::CursorExpired => {
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
            ControlPlaneErrorClass::Unknown => {
                homeboy_core::Error::internal_unexpected(error.message)
            }
        })
}

fn default_retry(
    run_id: &str,
    parameters: &ControlPlaneRetryParameters,
) -> homeboy_core::Result<crate::agent_task_service::AgentTaskRetryServiceResult> {
    let route = parameters.provider_route.as_ref().map(|route| {
        crate::agent_task_service::CookProviderRouteOverride {
            backend: route.backend.clone(),
            selector: route.selector.clone(),
            model: route.model.clone(),
            allow_provider_rotation: route.allow_provider_rotation,
            provider_rotations: route.provider_rotations,
        }
    });
    crate::agent_task_service::retry_with_provider_route_override(
        run_id,
        parameters.new_run_id.as_deref(),
        false,
        parameters.force,
        route.unwrap_or_default(),
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
    }
    if let Some(mission) = fanout_mission(record)? {
        resource.mission = Some(mission);
    }
    resource.state = run_state(record);
    resource.location = location(record);
    resource.placement = placement(record);
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
    use homeboy_control_plane_contract::{ControlPlaneEventPage, CONTROL_PLANE_EVENT_PAGE_SCHEMA};

    let (earliest_sequence, latest_sequence) = validate_event_stream(&run, &events)?;
    let after = cursor
        .map(|cursor| decode_event_cursor(cursor, &run))
        .transpose()?
        .unwrap_or(0);
    if cursor.is_some() && events.is_empty() {
        return Err(ControlPlaneError::cursor_expired(
            "control-plane event cursor has expired; the retained stream is empty",
        ));
    }
    if let Some(earliest) = earliest_sequence {
        if cursor.is_some() && after < earliest.saturating_sub(1) {
            return Err(ControlPlaneError::cursor_expired(format!(
                "control-plane event cursor has expired; earliest retained sequence is {earliest}"
            )));
        }
    }
    if latest_sequence.is_some_and(|latest| after > latest) {
        return Err(ControlPlaneError::invalid_argument(
            "control-plane event cursor is ahead of the retained stream",
        ));
    }
    let mut remaining = events.into_iter().filter(|event| event.sequence > after);
    let page_events: Vec<_> = remaining.by_ref().take(EVENT_PAGE_BOUND).collect();
    let has_more = remaining.next().is_some();
    let next_cursor = page_events
        .last()
        .map(|event| encode_event_cursor(&run, event.sequence))
        .or_else(|| cursor.cloned().map(Ok))
        .transpose()?;

    Ok(ControlPlaneEventPage {
        schema: CONTROL_PLANE_EVENT_PAGE_SCHEMA.to_string(),
        run,
        events: page_events,
        next_cursor,
        has_more,
    })
}

pub fn event_retention(
    run: RunId,
    events: &[homeboy_control_plane_contract::ControlPlaneEvent],
) -> Result<ControlPlaneEventRetention, ControlPlaneError> {
    let (earliest_sequence, latest_sequence) = validate_event_stream(&run, events)?;
    Ok(ControlPlaneEventRetention {
        schema: CONTROL_PLANE_EVENT_RETENTION_SCHEMA.to_string(),
        run,
        earliest_sequence,
        latest_sequence,
    })
}

fn validate_event_stream(
    run: &RunId,
    events: &[homeboy_control_plane_contract::ControlPlaneEvent],
) -> Result<(Option<u64>, Option<u64>), ControlPlaneError> {
    let mut previous_sequence = None;
    let mut event_ids = std::collections::BTreeSet::new();
    for event in events {
        if event.run != *run
            || event.sequence == 0
            || previous_sequence.is_some_and(|previous| event.sequence <= previous)
            || !event_ids.insert(event.event.clone())
        {
            return Err(ControlPlaneError::invalid_argument(
                "control-plane event stream is not strictly ordered and unique for this run",
            ));
        }
        previous_sequence = Some(event.sequence);
    }
    Ok((
        events.first().map(|event| event.sequence),
        events.last().map(|event| event.sequence),
    ))
}

fn encode_event_cursor(run: &RunId, sequence: u64) -> Result<EventCursor, ControlPlaneError> {
    let bytes = serde_json::to_vec(&EventCursorPayload {
        schema: EVENT_CURSOR_SCHEMA.to_string(),
        run_id: run.as_str().to_string(),
        sequence,
    })
    .map_err(|error| ControlPlaneError::unavailable(error.to_string()))?;
    EventCursor::new(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
        .map_err(|error| ControlPlaneError::invalid_argument(error.to_string()))
}

fn decode_event_cursor(cursor: &EventCursor, run: &RunId) -> Result<u64, ControlPlaneError> {
    if let Ok(sequence) = cursor.as_str().parse::<u64>() {
        return (sequence > 0).then_some(sequence).ok_or_else(|| {
            ControlPlaneError::invalid_argument("control-plane event cursor is invalid")
        });
    }
    if cursor.as_str().len() > RUN_CURSOR_BOUND {
        return Err(ControlPlaneError::invalid_argument(
            "control-plane event cursor exceeds the size bound",
        ));
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cursor.as_str())
        .map_err(|_| {
            ControlPlaneError::invalid_argument("control-plane event cursor is invalid")
        })?;
    let payload: EventCursorPayload = serde_json::from_slice(&bytes).map_err(|_| {
        ControlPlaneError::invalid_argument("control-plane event cursor is invalid")
    })?;
    if payload.schema != EVENT_CURSOR_SCHEMA
        || payload.run_id != run.as_str()
        || payload.sequence == 0
    {
        return Err(ControlPlaneError::invalid_argument(
            "control-plane event cursor is invalid for this run",
        ));
    }
    Ok(payload.sequence)
}

fn identities_for_record(
    record: &AgentTaskRunRecord,
) -> Result<Option<CanonicalControlPlaneIdentities>, ControlPlaneError> {
    canonical_control_plane_identities(record)
        .map_err(|error| ControlPlaneError::invalid_argument(error.message))
}

fn fanout_mission(record: &AgentTaskRunRecord) -> Result<Option<MissionId>, ControlPlaneError> {
    crate::agent_task_lifecycle::canonical_fanout_mission(&record.metadata)
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

fn placement(record: &AgentTaskRunRecord) -> Option<ControlPlaneRunPlacement> {
    use homeboy_lab_runner_contract::{EffectiveExecutionPlacement, Placement};

    let decision =
        serde_json::from_value::<homeboy_lab_runner_contract::ExecutionPlacementDecision>(
            record.metadata.get("execution_placement_decision")?.clone(),
        )
        .ok()?;
    if !decision.is_valid() {
        return None;
    }
    let selected = match decision.selected {
        EffectiveExecutionPlacement::Local if decision.runner.is_none() => {
            ControlPlaneRunPlacementSelected::Controller
        }
        EffectiveExecutionPlacement::Lab if decision.runner.is_some() => {
            ControlPlaneRunPlacementSelected::Runner
        }
        _ => return None,
    };
    let outcome = record
        .metadata
        .get("execution_placement_outcome")
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .filter(
            |outcome: &homeboy_lab_runner_contract::ExecutionPlacementOutcome| {
                outcome.decision_id == decision.decision_id
                    && decision.outcome(outcome.effective, outcome.runner_id.clone())
                        == Some(outcome.clone())
            },
        );
    ControlPlaneRunPlacement::new(
        decision.decision_id,
        match decision.requested {
            Placement::Auto => ControlPlaneRunPlacementRequested::Automatic,
            Placement::Local => ControlPlaneRunPlacementRequested::Controller,
            Placement::Lab | Placement::LabOrLocal => ControlPlaneRunPlacementRequested::Runner,
        },
        selected,
        outcome.map(|outcome| match outcome.effective {
            EffectiveExecutionPlacement::Local => ControlPlaneRunPlacementEffective::Controller,
            EffectiveExecutionPlacement::Lab => ControlPlaneRunPlacementEffective::Runner,
        }),
        decision.runner.map(|runner| runner.runner_id),
    )
    .ok()
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
        .or_else(|| {
            record
                .metadata
                .get("phase")
                .and_then(|value| value.as_str())
                .filter(|value| !value.trim().is_empty())
                .map(|value| bounded(value, STATE_BOUND))
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
            state: None,
            reason: None,
            retry: None,
        });
    }
    if let Some(reason) = record.stale_running_reason() {
        return Some(ControlPlaneBlocker {
            code: Some("stale".to_string()),
            message: redacted_bounded(reason, MESSAGE_BOUND),
            state: None,
            reason: None,
            retry: None,
        });
    }
    if let Some(admission) = record.metadata.get("unmaterialized_cook_admission") {
        if let Some((state, reason, retry)) = unmaterialized_admission_blocker(admission) {
            return Some(ControlPlaneBlocker {
                code: Some(state.clone()),
                message: reason.clone(),
                state: Some(state),
                reason: Some(reason),
                retry,
            });
        }
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
            state: None,
            reason: None,
            retry: None,
        });
    }
    if let Some(failure) = record.metadata.get("pre_execution_failure") {
        if let Some(message) = failure
            .get("message")
            .and_then(|value| value.as_str())
            .filter(|value| !value.trim().is_empty())
        {
            return Some(ControlPlaneBlocker {
                code: failure
                    .get("error_code")
                    .and_then(|value| value.as_str())
                    .filter(|value| !value.trim().is_empty())
                    .map(|value| bounded(value, STATE_BOUND)),
                message: redacted_bounded(message, MESSAGE_BOUND),
                state: None,
                reason: None,
                retry: None,
            });
        }
    }
    record
        .candidate_adoption
        .as_ref()
        .and_then(|adoption| adoption.terminal_error.as_deref())
        .filter(|value| !value.trim().is_empty())
        .map(|message| ControlPlaneBlocker {
            code: Some("adoption".to_string()),
            message: redacted_bounded(message, MESSAGE_BOUND),
            state: None,
            reason: None,
            retry: None,
        })
}

fn unmaterialized_admission_blocker(
    admission: &Value,
) -> Option<(String, String, Option<ControlPlaneAdmissionRetry>)> {
    let state = admission.get("state")?.as_str()?.trim();
    if !matches!(
        state,
        "queued" | "blocked_runner_unavailable" | "blocked_runner_stale" | "exhausted"
    ) {
        return None;
    }
    let reason = admission
        .get("reason")
        .and_then(Value::as_str)
        .filter(|reason| !reason.trim().is_empty())
        .map(|reason| redacted_bounded(reason, MESSAGE_BOUND))
        .unwrap_or_else(|| redacted_bounded(state, MESSAGE_BOUND));
    let Some(retry) = admission.get("retry") else {
        return Some((state.to_string(), reason, None));
    };
    if retry.get("policy").and_then(Value::as_str) != Some("bounded_exponential") {
        return Some((state.to_string(), reason, None));
    }
    let Some(max_attempts) = retry.get("max_attempts").and_then(Value::as_u64) else {
        return Some((state.to_string(), reason, None));
    };
    let attempts = admission
        .get("admission_attempts")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let next_attempt_at = retry
        .get("next_attempt_at")
        .and_then(Value::as_str)
        .and_then(parse_timestamp)
        .map(|timestamp| timestamp.to_rfc3339());
    let disposition = if state == "exhausted" || attempts >= max_attempts {
        ControlPlaneAdmissionRetryDisposition::Exhausted
    } else if next_attempt_at
        .as_deref()
        .and_then(parse_timestamp)
        .is_some_and(|timestamp| timestamp > Utc::now())
    {
        ControlPlaneAdmissionRetryDisposition::AutomaticReconciliationScheduled
    } else {
        ControlPlaneAdmissionRetryDisposition::AutomaticReconciliationDue
    };
    Some((
        state.to_string(),
        reason,
        Some(ControlPlaneAdmissionRetry {
            policy: "bounded_exponential".to_string(),
            attempts,
            max_attempts,
            next_attempt_at,
            disposition,
        }),
    ))
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
        .map(|evidence| {
            let kind = redacted_bounded(&evidence.kind, STATE_BOUND);
            let uri = redacted_reference_uri(&evidence.uri, URI_BOUND);
            ControlPlaneEvidenceRef {
                id: stable_reference_id("evidence", &[&kind, &uri]),
                kind,
                uri,
            }
        })
        .take(REF_BOUND)
        .collect()
}

fn artifact_refs(record: &AgentTaskRunRecord) -> Vec<ControlPlaneEvidenceRef> {
    record
        .artifact_refs
        .iter()
        .map(|artifact| {
            let task_id = redacted_bounded(&artifact.task_id, ID_BOUND);
            let kind = redacted_bounded(&artifact.kind, STATE_BOUND);
            let uri = redacted_reference_uri(&artifact.uri, URI_BOUND);
            let role = artifact
                .role
                .as_deref()
                .map(|role| redacted_bounded(role, STATE_BOUND))
                .unwrap_or_default();
            let semantic_key = artifact
                .semantic_key
                .as_deref()
                .map(|key| redacted_bounded(key, ID_BOUND))
                .unwrap_or_default();
            ControlPlaneEvidenceRef {
                id: stable_reference_id("artifact", &[&task_id, &kind, &uri, &role, &semantic_key]),
                kind,
                uri,
            }
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

fn stable_reference_id(prefix: &str, parts: &[&str]) -> String {
    let digest = homeboy_engine_primitives::content_hash::sha256_hex(parts.join("\0").as_bytes());
    format!("{prefix}-{}", &digest[..32])
}

fn redacted_reference_uri(value: &str, max: usize) -> String {
    let without_fragment = value.split_once('#').map_or(value, |(uri, _)| uri);
    bounded(
        &homeboy_core::redaction::RedactionPolicy::default().redact_url(without_fragment),
        max,
    )
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

fn generic_observation_run(
    store: &homeboy_core::observation::ObservationStore,
    record: &homeboy_core::observation::RunRecord,
) -> Result<ControlPlaneRun, ControlPlaneError> {
    let run = RunId::new(&record.id)
        .map_err(|error| ControlPlaneError::invalid_argument(error.to_string()))?;
    let mut resource = ControlPlaneRun::new(run);
    resource.mission = store
        .get_run_mission(&record.id)
        .map_err(map_lifecycle_error)?
        .map(MissionId::new)
        .transpose()
        .map_err(|error| ControlPlaneError::invalid_argument(error.to_string()))?;
    resource.state = generic_observation_state(&record.status);
    resource.phase = record
        .metadata_json
        .pointer("/control_plane/phase")
        .and_then(Value::as_str)
        .and_then(|value| nonempty_redacted_bounded(value, STATE_BOUND));
    resource.created_at = record.started_at.clone();
    resource.updated_at = record
        .finished_at
        .clone()
        .or_else(|| Some(record.started_at.clone()));
    resource.finished_at = record.finished_at.clone();
    resource.blocker = record
        .metadata_json
        .pointer("/control_plane/blocker")
        .and_then(Value::as_str)
        .map(|message| ControlPlaneBlocker {
            code: None,
            message: redacted_bounded(message, MESSAGE_BOUND),
            state: None,
            reason: None,
            retry: None,
        });
    resource.artifacts = generic_observation_artifacts(record);
    resource.action_eligibility = record
        .metadata_json
        .pointer("/control_plane/actions")
        .cloned()
        .map(|actions| {
            serde_json::from_value(actions).map(|actions| {
                let mut report =
                    homeboy_control_plane_contract::ControlPlaneActionEligibilityReport::new(
                        resource.run.clone(),
                    );
                report.actions = actions;
                report
            })
        })
        .transpose()
        .map_err(|error| {
            ControlPlaneError::invalid_argument(format!(
                "generic control-plane action eligibility is invalid: {error}"
            ))
        })?;
    Ok(resource)
}

fn bind_extension_owners(
    plan: &mut crate::agent_task_scheduler::AgentTaskPlan,
    providers: &[crate::agent_task_provider::AgentTaskExecutorProvider],
) {
    let owners = plan
        .tasks
        .iter()
        .filter_map(|request| {
            crate::agent_task_provider::resolve_provider_for_backend(
                providers,
                &request.executor.backend,
                request.executor.selector.as_deref(),
            )
            .resolved()
            .and_then(|provider| provider.extension_id.as_ref())
            .map(|extension_id| (request.task_id.clone(), Value::String(extension_id.clone())))
        })
        .collect::<serde_json::Map<_, _>>();
    if !plan.metadata.is_object() {
        plan.metadata = serde_json::json!({});
    }
    plan.metadata["control_plane"]["extension_owners"] = Value::Object(owners);
    plan.rebuild_homeboy_plan();
}

fn generic_observation_state(status: &str) -> ControlPlaneRunState {
    match status {
        "running" => ControlPlaneRunState::Running,
        "pass" => ControlPlaneRunState::Succeeded,
        "fail" | "error" => ControlPlaneRunState::Failed,
        "skipped" => ControlPlaneRunState::Skipped,
        _ => ControlPlaneRunState::Unknown,
    }
}

fn generic_task_state(value: &str) -> ControlPlaneState {
    match value {
        "queued" => ControlPlaneState::Queued,
        "blocked" => ControlPlaneState::Blocked,
        "running" => ControlPlaneState::Running,
        "succeeded" => ControlPlaneState::Succeeded,
        "partial_failure" => ControlPlaneState::PartialFailure,
        "failed" => ControlPlaneState::Failed,
        "cancelled" => ControlPlaneState::Cancelled,
        "timed_out" => ControlPlaneState::TimedOut,
        "skipped" => ControlPlaneState::Skipped,
        _ => ControlPlaneState::Unknown,
    }
}

fn generic_observation_artifacts(
    record: &homeboy_core::observation::RunRecord,
) -> Vec<ControlPlaneEvidenceRef> {
    record
        .metadata_json
        .pointer("/control_plane/artifacts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(REF_BOUND)
        .filter_map(|artifact| {
            Some(ControlPlaneEvidenceRef {
                id: nonempty_redacted_bounded(artifact.get("id")?.as_str()?, ID_BOUND)?,
                kind: nonempty_redacted_bounded(artifact.get("kind")?.as_str()?, STATE_BOUND)?,
                uri: nonempty_bounded(artifact.get("uri")?.as_str()?, URI_BOUND)
                    .map(|uri| redacted_reference_uri(&uri, URI_BOUND))?,
            })
        })
        .collect()
}

fn generic_observation_tasks(
    store: &homeboy_core::observation::ObservationStore,
    record: &homeboy_core::observation::RunRecord,
) -> Result<Vec<ControlPlaneTask>, ControlPlaneError> {
    let run = RunId::new(&record.id)
        .map_err(|error| ControlPlaneError::invalid_argument(error.to_string()))?;
    let mission = store
        .get_run_mission(&record.id)
        .map_err(map_lifecycle_error)?
        .ok_or_else(|| {
            ControlPlaneError::not_found(format!("control-plane run not found: {}", record.id))
        })?;
    let mission = Some(
        MissionId::new(mission)
            .map_err(|error| ControlPlaneError::invalid_argument(error.to_string()))?,
    );
    let mut tasks = record
        .metadata_json
        .pointer("/control_plane/tasks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|task| {
            let task_id = task.get("id").and_then(Value::as_str).ok_or_else(|| {
                ControlPlaneError::invalid_argument("generic control-plane task is missing its id")
            })?;
            Ok(ControlPlaneTask {
                schema: CONTROL_PLANE_TASK_SCHEMA.to_string(),
                mission: mission.clone(),
                run: run.clone(),
                task: TaskId::new(task_id)
                    .map_err(|error| ControlPlaneError::invalid_argument(error.to_string()))?,
                state: generic_task_state(
                    task.get("state")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown"),
                ),
            })
        })
        .collect::<Result<Vec<_>, ControlPlaneError>>()?;
    tasks.sort_by(|left, right| left.task.as_str().cmp(right.task.as_str()));
    if tasks.windows(2).any(|pair| pair[0].task == pair[1].task) {
        return Err(ControlPlaneError::invalid_argument(format!(
            "run {} contains duplicate task identities",
            record.id
        )));
    }
    Ok(tasks)
}

fn generic_observation_task(
    store: &homeboy_core::observation::ObservationStore,
    record: &homeboy_core::observation::RunRecord,
    requested: &TaskId,
) -> Result<ControlPlaneTask, ControlPlaneError> {
    generic_observation_tasks(store, record)?
        .into_iter()
        .find(|task| &task.task == requested)
        .ok_or_else(|| {
            ControlPlaneError::not_found(format!(
                "task not found in run {}: {requested}",
                record.id
            ))
        })
}

fn generic_observation_attempt(
    store: &homeboy_core::observation::ObservationStore,
    record: &homeboy_core::observation::RunRecord,
    task: &TaskId,
) -> Result<ControlPlaneAttempt, ControlPlaneError> {
    let task_resource = generic_observation_task(store, record, task)?;
    Ok(ControlPlaneAttempt {
        schema: CONTROL_PLANE_ATTEMPT_SCHEMA.to_string(),
        run: task_resource.run.clone(),
        task: task.clone(),
        attempt: AttemptId::new(format!("{}:{task}:attempt-1", record.id))
            .map_err(|error| ControlPlaneError::invalid_argument(error.to_string()))?,
        attempt_number: 1,
        state: task_resource.state,
        started_at: record.started_at.clone(),
        finished_at: record.finished_at.clone(),
        execution: Some(
            ExecutionId::new(format!("{}:{task}:attempt-1:execution", record.id))
                .map_err(|error| ControlPlaneError::invalid_argument(error.to_string()))?,
        ),
    })
}

fn generic_observation_execution(
    store: &homeboy_core::observation::ObservationStore,
    record: &homeboy_core::observation::RunRecord,
    task: &TaskId,
) -> Result<ControlPlaneExecution, ControlPlaneError> {
    let attempt = generic_observation_attempt(store, record, task)?;
    Ok(ControlPlaneExecution {
        schema: CONTROL_PLANE_EXECUTION_SCHEMA.to_string(),
        run: attempt.run,
        task: attempt.task,
        attempt: attempt.attempt,
        execution: attempt.execution.expect("generic attempt has execution"),
        state: attempt.state,
        started_at: attempt.started_at,
        finished_at: attempt.finished_at,
    })
}

struct RegisteredProvider;

impl ControlPlaneProvider for RegisteredProvider {
    fn capabilities(&self) -> ControlPlaneCapabilities {
        OrchestrationService::<LifecycleStoreLookup>::capabilities()
    }

    fn authorize_extension_execution(
        &self,
        extension_id: &str,
        run: &RunId,
        task: &TaskId,
    ) -> Result<bool, ControlPlaneError> {
        let store = AgentTaskLifecycleStore::from_environment()
            .map_err(|error| ControlPlaneError::unavailable(error.message))?;
        let plan = store
            .read_controller_plan(run.as_str())
            .map_err(map_lifecycle_error)?;
        let request = plan
            .tasks
            .iter()
            .find(|request| request.task_id == task.as_str())
            .ok_or_else(|| {
                ControlPlaneError::not_found(format!(
                    "control-plane task not found in run {run}: {task}"
                ))
            })?;
        Ok(plan
            .metadata
            .pointer(&format!(
                "/control_plane/extension_owners/{}",
                request.task_id.replace('~', "~0").replace('/', "~1")
            ))
            .and_then(Value::as_str)
            == Some(extension_id))
    }

    fn run(&self, requested_id: &RunId) -> Result<ControlPlaneRun, ControlPlaneError> {
        let store = AgentTaskLifecycleStore::from_environment()
            .map_err(|error| ControlPlaneError::unavailable(error.message))?;
        let observation = store
            .open_observation_readonly()
            .map_err(map_lifecycle_error)?;
        if let Some(record) = observation
            .get_run(requested_id.as_str())
            .map_err(map_lifecycle_error)?
        {
            if record.kind != "agent-task" {
                if observation
                    .get_run_mission(&record.id)
                    .map_err(map_lifecycle_error)?
                    .is_none()
                {
                    return Err(ControlPlaneError::not_found(format!(
                        "control-plane run not found: {requested_id}"
                    )));
                }
                return generic_observation_run(&observation, &record);
            }
        }
        OrchestrationService::new(LifecycleStoreLookup::new(store)).run(requested_id)
    }

    fn mission(&self, requested_id: &MissionId) -> Result<ControlPlaneMission, ControlPlaneError> {
        let store = AgentTaskLifecycleStore::from_environment()
            .map_err(|error| ControlPlaneError::unavailable(error.message))?;
        OrchestrationService::new(LifecycleStoreLookup::new(store)).mission(requested_id)
    }

    fn missions(
        &self,
        request: &ControlPlaneMissionListRequest,
    ) -> Result<ControlPlaneMissionPage, ControlPlaneError> {
        let store = AgentTaskLifecycleStore::from_environment()
            .map_err(|error| ControlPlaneError::unavailable(error.message))?;
        OrchestrationService::new(LifecycleStoreLookup::new(store)).missions(request)
    }

    fn runs(
        &self,
        request: &ControlPlaneRunListRequest,
    ) -> Result<ControlPlaneRunPage, ControlPlaneError> {
        request.validate()?;
        let store = AgentTaskLifecycleStore::from_environment()
            .map_err(|error| ControlPlaneError::unavailable(error.message))?;
        let decoded = request.cursor.as_ref().map(decode_run_cursor).transpose()?;
        if let Some((_, cursor_mission)) = &decoded {
            if cursor_mission.as_ref() != request.mission.as_ref() {
                return Err(ControlPlaneError::invalid_argument(
                    "control-plane run cursor does not match the mission filter",
                ));
            }
        }
        let after = decoded.map(|(position, _)| homeboy_core::observation::RunCursor {
            started_at: position.started_at,
            id: position.run_id,
        });
        let observation = store
            .open_observation_readonly()
            .map_err(map_lifecycle_error)?;
        let page = if let Some(mission) = request.mission.as_ref() {
            observation.list_mission_runs_page(
                mission.as_str(),
                after.as_ref(),
                request.limit as usize,
            )
        } else {
            observation.list_control_plane_runs_page(after.as_ref(), request.limit as usize)
        }
        .map_err(map_lifecycle_error)?;
        let lookup = LifecycleStoreLookup::new(store);
        let runs = page
            .runs
            .iter()
            .map(|record| {
                if record.kind == "agent-task" {
                    let run = RunId::new(&record.id)
                        .map_err(|error| ControlPlaneError::invalid_argument(error.to_string()))?;
                    let snapshot = lookup.get(&run)?.ok_or_else(|| {
                        ControlPlaneError::not_found(format!("run not found: {run}"))
                    })?;
                    project_record(&snapshot.record, snapshot.plan.as_ref())
                } else {
                    generic_observation_run(&observation, record)
                }
            })
            .collect::<Result<Vec<_>, ControlPlaneError>>()?;
        let next_cursor = page
            .next_cursor
            .as_ref()
            .map(|cursor| {
                encode_run_cursor(
                    &RunPagePosition {
                        started_at: cursor.started_at.clone(),
                        run_id: cursor.id.clone(),
                    },
                    request.mission.as_ref(),
                )
            })
            .transpose()?;
        Ok(ControlPlaneRunPage {
            schema: CONTROL_PLANE_RUN_PAGE_SCHEMA.to_string(),
            runs,
            has_more: next_cursor.is_some(),
            next_cursor,
        })
    }

    fn task(&self, run: &RunId, task: &TaskId) -> Result<ControlPlaneTask, ControlPlaneError> {
        let store = AgentTaskLifecycleStore::from_environment()
            .map_err(|error| ControlPlaneError::unavailable(error.message))?;
        let observation = store
            .open_observation_readonly()
            .map_err(map_lifecycle_error)?;
        if let Some(record) = observation
            .get_run(run.as_str())
            .map_err(map_lifecycle_error)?
        {
            if record.kind != "agent-task" {
                return generic_observation_task(&observation, &record, task);
            }
        }
        OrchestrationService::new(LifecycleStoreLookup::new(store)).task(run, task)
    }

    fn tasks(
        &self,
        run: &RunId,
        request: &ControlPlaneTaskListRequest,
    ) -> Result<ControlPlaneTaskPage, ControlPlaneError> {
        request.validate()?;
        let store = AgentTaskLifecycleStore::from_environment()
            .map_err(|error| ControlPlaneError::unavailable(error.message))?;
        let observation = store
            .open_observation_readonly()
            .map_err(map_lifecycle_error)?;
        if let Some(record) = observation
            .get_run(run.as_str())
            .map_err(map_lifecycle_error)?
        {
            if record.kind != "agent-task" {
                let after = request
                    .cursor
                    .as_ref()
                    .map(|cursor| decode_task_cursor(cursor, run))
                    .transpose()?;
                let mut tasks = generic_observation_tasks(&observation, &record)?;
                if let Some(after) = after {
                    tasks.retain(|task| task.task.as_str() > after.as_str());
                }
                let has_more = tasks.len() > request.limit as usize;
                tasks.truncate(request.limit as usize);
                let next_cursor = has_more
                    .then(|| tasks.last().map(|task| encode_task_cursor(run, &task.task)))
                    .flatten()
                    .transpose()?;
                return Ok(ControlPlaneTaskPage {
                    schema: CONTROL_PLANE_TASK_PAGE_SCHEMA.to_string(),
                    run: run.clone(),
                    tasks,
                    has_more,
                    next_cursor,
                });
            }
        }
        OrchestrationService::new(LifecycleStoreLookup::new(store)).tasks(run, request)
    }

    fn attempt(
        &self,
        run: &RunId,
        task: &TaskId,
        attempt_number: u32,
    ) -> Result<ControlPlaneAttempt, ControlPlaneError> {
        let store = AgentTaskLifecycleStore::from_environment()
            .map_err(|error| ControlPlaneError::unavailable(error.message))?;
        let observation = store
            .open_observation_readonly()
            .map_err(map_lifecycle_error)?;
        if let Some(record) = observation
            .get_run(run.as_str())
            .map_err(map_lifecycle_error)?
        {
            if record.kind != "agent-task" {
                if attempt_number != 1 {
                    return Err(ControlPlaneError::not_found(format!(
                        "attempt {attempt_number} not found for task {task}"
                    )));
                }
                return generic_observation_attempt(&observation, &record, task);
            }
        }
        OrchestrationService::new(LifecycleStoreLookup::new(store)).attempt(
            run,
            task,
            attempt_number,
        )
    }

    fn attempts(
        &self,
        run: &RunId,
        task: &TaskId,
        request: &ControlPlaneAttemptListRequest,
    ) -> Result<ControlPlaneAttemptPage, ControlPlaneError> {
        request.validate()?;
        let store = AgentTaskLifecycleStore::from_environment()
            .map_err(|error| ControlPlaneError::unavailable(error.message))?;
        let observation = store
            .open_observation_readonly()
            .map_err(map_lifecycle_error)?;
        if let Some(record) = observation
            .get_run(run.as_str())
            .map_err(map_lifecycle_error)?
        {
            if record.kind != "agent-task" {
                let after = request
                    .cursor
                    .as_ref()
                    .map(|cursor| decode_attempt_cursor(cursor, run, task))
                    .transpose()?;
                let attempts = if after.is_some() {
                    Vec::new()
                } else {
                    vec![generic_observation_attempt(&observation, &record, task)?]
                };
                return Ok(ControlPlaneAttemptPage {
                    schema: CONTROL_PLANE_ATTEMPT_PAGE_SCHEMA.to_string(),
                    run: run.clone(),
                    task: task.clone(),
                    attempts,
                    has_more: false,
                    next_cursor: None,
                });
            }
        }
        OrchestrationService::new(LifecycleStoreLookup::new(store)).attempts(run, task, request)
    }

    fn execution(
        &self,
        run: &RunId,
        task: &TaskId,
        attempt_number: u32,
        execution: &ExecutionId,
    ) -> Result<ControlPlaneExecution, ControlPlaneError> {
        let store = AgentTaskLifecycleStore::from_environment()
            .map_err(|error| ControlPlaneError::unavailable(error.message))?;
        let observation = store
            .open_observation_readonly()
            .map_err(map_lifecycle_error)?;
        if let Some(record) = observation
            .get_run(run.as_str())
            .map_err(map_lifecycle_error)?
        {
            if record.kind != "agent-task" {
                if attempt_number != 1 {
                    return Err(ControlPlaneError::not_found(format!(
                        "attempt {attempt_number} not found for task {task}"
                    )));
                }
                let projected = generic_observation_execution(&observation, &record, task)?;
                if &projected.execution != execution {
                    return Err(ControlPlaneError::not_found(format!(
                        "execution {execution} not found for task {task}"
                    )));
                }
                return Ok(projected);
            }
        }
        OrchestrationService::new(LifecycleStoreLookup::new(store)).execution(
            run,
            task,
            attempt_number,
            execution,
        )
    }

    fn executions(
        &self,
        run: &RunId,
        task: &TaskId,
        attempt_number: u32,
    ) -> Result<ControlPlaneExecutionPage, ControlPlaneError> {
        let store = AgentTaskLifecycleStore::from_environment()
            .map_err(|error| ControlPlaneError::unavailable(error.message))?;
        let observation = store
            .open_observation_readonly()
            .map_err(map_lifecycle_error)?;
        if let Some(record) = observation
            .get_run(run.as_str())
            .map_err(map_lifecycle_error)?
        {
            if record.kind != "agent-task" {
                if attempt_number != 1 {
                    return Err(ControlPlaneError::not_found(format!(
                        "attempt {attempt_number} not found for task {task}"
                    )));
                }
                let attempt = generic_observation_attempt(&observation, &record, task)?;
                return Ok(ControlPlaneExecutionPage {
                    schema: CONTROL_PLANE_EXECUTION_PAGE_SCHEMA.to_string(),
                    run: run.clone(),
                    task: task.clone(),
                    attempt: attempt.attempt,
                    executions: vec![generic_observation_execution(&observation, &record, task)?],
                });
            }
        }
        OrchestrationService::new(LifecycleStoreLookup::new(store)).executions(
            run,
            task,
            attempt_number,
        )
    }

    fn reference(
        &self,
        run: &RunId,
        reference_type: ControlPlaneReferenceType,
        reference: &ReferenceId,
    ) -> Result<ControlPlaneReference, ControlPlaneError> {
        let store = AgentTaskLifecycleStore::from_environment()
            .map_err(|error| ControlPlaneError::unavailable(error.message))?;
        OrchestrationService::new(LifecycleStoreLookup::new(store)).reference(
            run,
            reference_type,
            reference,
        )
    }

    fn references(
        &self,
        run: &RunId,
        reference_type: ControlPlaneReferenceType,
    ) -> Result<ControlPlaneReferencePage, ControlPlaneError> {
        let store = AgentTaskLifecycleStore::from_environment()
            .map_err(|error| ControlPlaneError::unavailable(error.message))?;
        OrchestrationService::new(LifecycleStoreLookup::new(store)).references(run, reference_type)
    }

    fn register_reference(
        &self,
        run: &RunId,
        reference_type: ControlPlaneReferenceType,
        request: &ControlPlaneReferenceRegistration,
    ) -> Result<ControlPlaneReference, ControlPlaneError> {
        let store = AgentTaskLifecycleStore::from_environment()
            .map_err(|error| ControlPlaneError::unavailable(error.message))?;
        OrchestrationService::new(LifecycleStoreLookup::new(store)).register_reference(
            run,
            reference_type,
            request,
        )
    }

    fn submit(
        &self,
        request: &ControlPlaneSubmissionRequest,
    ) -> Result<ControlPlaneSubmissionAcknowledgement, ControlPlaneError> {
        if request.run.as_str().len() > 256
            || homeboy_core::paths::sanitize_path_segment(request.run.as_str())
                != request.run.as_str()
        {
            return Err(ControlPlaneError::invalid_argument(
                "staged control-plane submission requires a canonical path-segment run id of at most 256 bytes",
            ));
        }
        let store = AgentTaskLifecycleStore::from_environment()
            .map_err(|error| ControlPlaneError::unavailable(error.message))?;
        let plan_path = store.controller_plan_path(request.run.as_str());
        match plan_path.try_exists() {
            Ok(true) => {}
            Ok(false) => {
                return Err(ControlPlaneError::not_found(format!(
                    "prepared controller plan not found for run: {}",
                    request.run
                )))
            }
            Err(error) => return Err(ControlPlaneError::unavailable(error.to_string())),
        }
        let mut plan = store
            .read_controller_plan(request.run.as_str())
            .map_err(map_lifecycle_error)?;
        let executor = crate::agent_task_provider::ExtensionProviderAgentTaskExecutor::discover();
        let already_admitted = store
            .open_observation_readonly()
            .and_then(|observation| observation.get_run(request.run.as_str()))
            .map_err(map_lifecycle_error)?
            .is_some();
        if !already_admitted {
            bind_extension_owners(&mut plan, executor.providers());
            store
                .write_controller_plan(request.run.as_str(), &plan)
                .map_err(map_lifecycle_error)?;
        }
        let prepared = crate::agent_task_submission_service::PreparedAgentTaskSubmission::new(plan)
            .with_lifecycle_store(store);
        let outcome = if request.queue_only {
            crate::agent_task_submission_service::queue_prepared_plan(request, prepared)
        } else {
            crate::agent_task_submission_service::submit_prepared_plan(
                request,
                prepared,
                std::sync::Arc::new(executor),
            )
        }
        .map_err(map_lifecycle_error)?;
        Ok(outcome.acknowledgement)
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

    fn append_event(
        &self,
        requested_id: &RunId,
        request: &ControlPlaneEventAppendRequest,
    ) -> Result<homeboy_control_plane_contract::ControlPlaneEvent, ControlPlaneError> {
        validate_external_event_append_request(request)?;
        let store = AgentTaskLifecycleStore::from_environment()
            .map_err(|error| ControlPlaneError::unavailable(error.message))?;
        append_event_in_store(&store, requested_id, request)
    }

    fn event_retention(
        &self,
        requested_id: &RunId,
    ) -> Result<ControlPlaneEventRetention, ControlPlaneError> {
        let store = AgentTaskLifecycleStore::from_environment()
            .map_err(|error| ControlPlaneError::unavailable(error.message))?;
        OrchestrationService::new(LifecycleStoreLookup::new(store)).event_retention(requested_id)
    }

    fn execute_action(
        &self,
        requested_id: &RunId,
        request: &ControlPlaneActionRequest,
    ) -> Result<ControlPlaneActionAcknowledgement, ControlPlaneError> {
        validate_action_request(request)?;
        let store = AgentTaskLifecycleStore::from_environment()
            .map_err(|error| ControlPlaneError::unavailable(error.message))?;
        let observation = store
            .open_observation_initialized()
            .map_err(map_lifecycle_error)?;
        if let Some(record) = observation
            .get_run(requested_id.as_str())
            .map_err(map_lifecycle_error)?
        {
            if record.kind != "agent-task" {
                let resource = generic_observation_run(&observation, &record)?;
                if resource.mission.is_none() {
                    return Err(ControlPlaneError::not_found(format!(
                        "control-plane run not found: {requested_id}"
                    )));
                }
                let available = resource.action_eligibility.as_ref().is_some_and(|report| {
                    report.actions.iter().any(|eligibility| {
                        eligibility.action == request.action
                            && eligibility.availability == ControlPlaneActionAvailability::Available
                    })
                });
                if !available {
                    if let Some(acknowledgement) =
                        homeboy_core::control_plane::replay_delegated_action(
                            &observation,
                            requested_id,
                            request,
                        )?
                    {
                        return Ok(acknowledgement);
                    }
                    return Err(ControlPlaneError::invalid_argument(format!(
                        "control-plane action is unavailable for run {requested_id}"
                    )));
                }
                return homeboy_core::control_plane::execute_delegated_action(
                    &observation,
                    &record,
                    request,
                    || {
                        let current = observation
                            .get_run(requested_id.as_str())
                            .map_err(map_lifecycle_error)?
                            .ok_or_else(|| {
                                ControlPlaneError::not_found(format!(
                                    "control-plane run not found: {requested_id}"
                                ))
                            })?;
                        generic_observation_run(&observation, &current)
                    },
                )?
                .ok_or_else(|| {
                    ControlPlaneError::invalid_argument(format!(
                        "control-plane actions are unavailable for run kind '{}'",
                        record.kind
                    ))
                });
            }
        }
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
        bounded_review_evidence, decode_event_cursor, decode_mission_cursor, encode_event_cursor,
        encode_mission_cursor, event_page, live_provider_liveness, normalize_event_references,
        observed_file_timestamp, phase, project_record, references_for_record,
        register_reference_in_store, review_failure_reasons, validate_event_scope,
        validate_external_event_append_request, LifecycleStoreLookup, OrchestrationService,
        RegisteredProvider, RunListLookup, RunLookup, RunPagePosition, RunSnapshot,
        RunSnapshotPage, REVIEW_EVIDENCE_BOUND,
    };
    use crate::agent_task_lifecycle::{
        claim_operation_with_intent_in_store, operation_claim_in_store, AgentTaskArtifactRef,
        AgentTaskLifecycleStore, AgentTaskRunRecord, AgentTaskRunState, AgentTaskRunTask,
        ClaimOutcome, ClaimState,
    };
    use crate::agent_task_schedule::AgentTaskPlan;
    use crate::agent_tasks::AgentTaskState;
    use homeboy_control_plane_contract::{
        ControlPlaneAction, ControlPlaneActionAvailability, ControlPlaneActionOutcome,
        ControlPlaneActionPayload, ControlPlaneActionRequest,
        ControlPlaneAdmissionRetryDisposition, ControlPlaneAttemptListRequest,
        ControlPlaneCancelDisposition, ControlPlaneCancelResult, ControlPlaneErrorClass,
        ControlPlaneEvent, ControlPlaneEventAppendRequest, ControlPlaneEventSource,
        ControlPlaneEvidenceRef, ControlPlaneMissionListRequest, ControlPlaneOperation,
        ControlPlaneReferenceRegistration, ControlPlaneReferenceType, ControlPlaneRunListRequest,
        ControlPlaneRunReviewRequest, ControlPlaneRunState, ControlPlaneState,
        ControlPlaneSubmissionRequest, ControlPlaneTaskListRequest, EventCursor, EventId,
        ExecutionId, MissionId, ReferenceId, RunCursor, RunId, TaskId,
        CONTROL_PLANE_ACTION_ELIGIBILITY_SCHEMA, CONTROL_PLANE_ACTION_REQUEST_SCHEMA,
        CONTROL_PLANE_CANCEL_PARAMETERS_SCHEMA, CONTROL_PLANE_EVENT_APPEND_REQUEST_SCHEMA,
        CONTROL_PLANE_EVENT_SCHEMA, CONTROL_PLANE_PROMOTE_PARAMETERS_SCHEMA,
        CONTROL_PLANE_PROMOTE_RESULT_SCHEMA, CONTROL_PLANE_REFERENCE_REGISTRATION_SCHEMA,
        CONTROL_PLANE_RESUME_RESULT_SCHEMA, CONTROL_PLANE_RETRY_PARAMETERS_SCHEMA,
        CONTROL_PLANE_RETRY_RESULT_SCHEMA, CONTROL_PLANE_RUN_SCHEMA,
        CONTROL_PLANE_SUBMISSION_REQUEST_SCHEMA,
    };
    use homeboy_core::control_plane::ControlPlaneProvider;
    use homeboy_core::run_lifecycle_record::RunHeartbeat;
    use homeboy_core::test_support::with_isolated_home;
    use serde_json::{json, Value};
    use std::cell::Cell;
    use std::collections::BTreeMap;

    const AGENT_TASK_COOK: &str = "agent-task-301a2b9a-a63d-446b-a918-e21b2ff6421e";
    const AGENT_TASK_RUN: &str =
        "agent-task-301a2b9a-a63d-446b-a918-e21b2ff6421e-attempt-1-ea6a6751";

    fn interrupt_action_after_effect(
        service: &OrchestrationService<LifecycleStoreLookup>,
        run: &RunId,
        request: &ControlPlaneActionRequest,
        effect: impl FnOnce(),
    ) -> String {
        let record = service
            .lookup
            .store
            .read_record(run.as_str())
            .expect("action record");
        let operation_key = format!(
            "control-plane-action:{}:{}",
            super::action_name(request.action),
            request.idempotency_key
        );
        let intent = serde_json::to_value(request).expect("action intent");
        assert_eq!(
            claim_operation_with_intent_in_store(
                &service.lookup.store,
                run.as_str(),
                &operation_key,
                super::ACTION_LEASE,
                &intent,
            )
            .expect("action claim"),
            ClaimOutcome::Acquired
        );
        let accepted_at =
            operation_claim_in_store(&service.lookup.store, run.as_str(), &operation_key)
                .expect("read action claim")
                .and_then(|claim| claim.accepted_at)
                .expect("durable accepted timestamp");
        super::append_action_event_in_store(
            &service.lookup.store,
            &record,
            request,
            &operation_key,
            "action.accepted",
            &accepted_at,
            json!({
                "operation_digest": super::action_operation_digest(&operation_key),
                "action": request.action,
                "acknowledgement": format!(
                    "{}:action:{}:{}",
                    record.run_id,
                    super::action_name(request.action),
                    request.idempotency_key
                ),
                "actor": request.actor,
                "expected_updated_at": request.expected_updated_at,
                "confirmed": request.confirmed,
                "parameters": request.parameters,
            }),
        )
        .expect("accepted receipt");
        effect();
        service
            .lookup
            .store
            .mutate_record(run.as_str(), |record| {
                let claim = record.metadata["cook_operation_claims"]
                    .as_array_mut()
                    .and_then(|claims| {
                        claims
                            .iter_mut()
                            .find(|claim| claim["operation_key"] == json!(operation_key.as_str()))
                    })
                    .expect("action claim");
                claim["owner_pid"] = json!(u32::MAX);
                true
            })
            .expect("simulate dead action owner");
        operation_key
    }

    #[derive(Clone)]
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

    impl RunListLookup for MapLookup {
        fn list(
            &self,
            mission: Option<&MissionId>,
            after: Option<&RunPagePosition>,
            limit: usize,
        ) -> Result<RunSnapshotPage, homeboy_control_plane_contract::ControlPlaneError> {
            let mut snapshots = self.snapshots.values().cloned().collect::<Vec<_>>();
            if let Some(mission) = mission {
                snapshots = snapshots
                    .into_iter()
                    .map(|snapshot| {
                        let projected = project_record(&snapshot.record, snapshot.plan.as_ref())?;
                        Ok((snapshot, projected.mission))
                    })
                    .collect::<Result<Vec<_>, homeboy_control_plane_contract::ControlPlaneError>>()?
                    .into_iter()
                    .filter_map(|(snapshot, projected_mission)| {
                        (projected_mission.as_ref() == Some(mission)).then_some(snapshot)
                    })
                    .collect();
            }
            snapshots.sort_by(|left, right| {
                right
                    .record
                    .submitted_at
                    .cmp(&left.record.submitted_at)
                    .then_with(|| right.record.run_id.cmp(&left.record.run_id))
            });
            if let Some(after) = after {
                snapshots.retain(|snapshot| {
                    snapshot.record.submitted_at < after.started_at
                        || (snapshot.record.submitted_at == after.started_at
                            && snapshot.record.run_id < after.run_id)
                });
            }
            let has_more = snapshots.len() > limit;
            snapshots.truncate(limit);
            let next_position = has_more.then(|| {
                let last = snapshots.last().expect("nonempty truncated page");
                RunPagePosition {
                    started_at: last.record.submitted_at.clone(),
                    run_id: last.record.run_id.clone(),
                }
            });
            Ok(RunSnapshotPage {
                snapshots,
                next_position,
            })
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

    fn runner_placement_decision(
        requested: homeboy_lab_runner_contract::Placement,
        fallback: bool,
    ) -> homeboy_lab_runner_contract::ExecutionPlacementDecision {
        use homeboy_lab_runner_contract::{
            EffectiveExecutionPlacement, ExecutionPlacementFallback, ExecutionPlacementIdentity,
            ExecutionPlacementOverrideAuthorization, ExecutionPlacementRequirement,
            ExecutionPlacementRunnerSelection, RunnerSelectionSource,
        };

        homeboy_lab_runner_contract::ExecutionPlacementDecision::new(
            "route",
            "1",
            ExecutionPlacementIdentity {
                repository: "repo".to_string(),
                workspace: "workspace".to_string(),
                task: "task".to_string(),
                candidate: None,
                base: None,
            },
            requested,
            if fallback {
                ExecutionPlacementRequirement::Either
            } else {
                ExecutionPlacementRequirement::Lab
            },
            EffectiveExecutionPlacement::Lab,
            Some(ExecutionPlacementRunnerSelection {
                runner_id: "runner-1".to_string(),
                source: RunnerSelectionSource::Policy,
            }),
            ExecutionPlacementFallback {
                local_allowed: fallback,
                reason: None,
            },
            ExecutionPlacementOverrideAuthorization {
                authorized: false,
                authority: None,
            },
        )
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

    fn event_append_request() -> ControlPlaneEventAppendRequest {
        ControlPlaneEventAppendRequest {
            schema: CONTROL_PLANE_EVENT_APPEND_REQUEST_SCHEMA.to_string(),
            idempotency_key: "progress-1".to_string(),
            actor: "broker:controller".to_string(),
            kind: "task.progress".to_string(),
            source: ControlPlaneEventSource {
                component: "runner".to_string(),
                instance: None,
            },
            occurred_at: None,
            task: None,
            attempt: None,
            execution: None,
            data: json!({}),
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
        let cursor = first.next_cursor.as_ref().expect("next cursor");
        assert_ne!(cursor.as_str(), "100");
        assert_eq!(decode_event_cursor(cursor, &run).expect("cursor"), 100);

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
    fn event_cursors_are_run_bound_and_expire_before_retention() {
        let run = RunId::new("run-events").expect("run");
        let other = RunId::new("other-run").expect("other run");
        let wrong_run = encode_event_cursor(&other, 4).expect("cursor");
        let error = event_page(run.clone(), vec![event(&run, 5)], Some(&wrong_run))
            .expect_err("run-bound cursor");
        assert_eq!(error.class, ControlPlaneErrorClass::InvalidArgument);

        let expired = encode_event_cursor(&run, 2).expect("expired cursor");
        let error = event_page(run.clone(), vec![event(&run, 5)], Some(&expired))
            .expect_err("expired cursor");
        assert_eq!(error.class, ControlPlaneErrorClass::CursorExpired);
        assert_eq!(error.http_status(), 410);

        let retained_boundary = encode_event_cursor(&run, 4).expect("retained boundary");
        let page = event_page(run.clone(), vec![event(&run, 5)], Some(&retained_boundary))
            .expect("retained cursor boundary");
        assert_eq!(page.events[0].sequence, 5);

        let legacy = EventCursor::new("4").expect("legacy cursor");
        let page =
            event_page(run.clone(), vec![event(&run, 5)], Some(&legacy)).expect("legacy v1 cursor");
        assert_eq!(page.events[0].sequence, 5);

        let error = event_page(run, Vec::new(), Some(&retained_boundary))
            .expect_err("fully evicted stream");
        assert_eq!(error.class, ControlPlaneErrorClass::CursorExpired);
    }

    #[test]
    fn appended_event_scope_is_bound_to_the_exact_run_graph() {
        let mut record = record(AGENT_TASK_RUN);
        record.tasks.push(AgentTaskRunTask {
            task_id: "review".to_string(),
            state: AgentTaskState::Succeeded,
            backend: "claude".to_string(),
            selector: None,
            model: None,
            provider_ref: None,
        });
        let attempt = format!("{AGENT_TASK_RUN}:review:1");
        let execution = format!("{attempt}:execution");
        record.metadata["provider_executions"] = json!([{
            "task_id": "review",
            "attempt": 1,
            "owner_identity": attempt,
            "execution_identity": execution,
            "state": "succeeded",
            "started_at": "2026-01-01T00:00:00Z",
            "finished_at": "2026-01-01T00:01:00Z"
        }]);
        let mut request = event_append_request();
        request.task = Some(TaskId::new("review").expect("task"));
        request.attempt = Some(
            homeboy_control_plane_contract::AttemptId::new(format!("{AGENT_TASK_RUN}:review:1"))
                .expect("attempt"),
        );
        request.execution = Some(ExecutionId::new(execution).expect("execution"));
        validate_event_scope(&record, &request).expect("exact scope");

        request.execution = Some(ExecutionId::new("foreign:execution").expect("foreign"));
        assert_eq!(
            validate_event_scope(&record, &request)
                .expect_err("foreign execution")
                .class,
            ControlPlaneErrorClass::InvalidArgument
        );
        request.task = None;
        assert_eq!(
            validate_event_scope(&record, &request)
                .expect_err("orphan identities")
                .class,
            ControlPlaneErrorClass::InvalidArgument
        );
    }

    #[test]
    fn appended_event_references_are_redacted_and_bounded_before_persistence() {
        let mut references = vec![ControlPlaneEvidenceRef {
            id: format!("evidence-token=secret-{}", "x".repeat(200)),
            kind: format!("transcript-token=secret-{}", "x".repeat(100)),
            uri: "https://user:password@example.invalid/log?token=secret#fragment".to_string(),
        }];
        normalize_event_references(&mut references).expect("normalize references");
        let encoded = serde_json::to_string(&references).expect("references");
        assert!(!encoded.contains("secret"));
        assert!(!encoded.contains("password"));
        assert!(!encoded.contains("fragment"));
        assert!(references[0].id.len() <= super::ID_BOUND);
        assert!(references[0].kind.len() <= super::STATE_BOUND);
        assert!(references[0].uri.len() <= super::URI_BOUND);
    }

    #[test]
    fn appended_event_data_uses_structured_secret_redaction() {
        let mut request = event_append_request();
        request.data = json!({
            "access_token": "secret-value",
            "message": "token=inline-secret"
        });
        request.data = homeboy_core::redaction::redact_json(&request.data);
        let encoded = serde_json::to_string(&request.data).expect("data");
        assert!(!encoded.contains("secret-value"));
        assert!(!encoded.contains("inline-secret"));
        assert_eq!(request.data["access_token"], "[REDACTED]");
    }

    #[test]
    fn external_event_append_cannot_claim_controller_action_namespaces() {
        let mut reserved_key = event_append_request();
        reserved_key.idempotency_key = "homeboy-internal-action:action.accepted:digest".to_string();
        let mut reserved_kind = event_append_request();
        reserved_kind.kind = "action.accepted".to_string();
        let mut reserved_source = event_append_request();
        reserved_source.source.component = "control-plane".to_string();

        for request in [reserved_key, reserved_kind, reserved_source] {
            assert_eq!(
                validate_external_event_append_request(&request)
                    .expect_err("reserved namespace")
                    .class,
                ControlPlaneErrorClass::InvalidArgument
            );
        }
    }

    #[test]
    fn event_pages_reject_non_monotonic_or_foreign_streams() {
        let run = RunId::new("run-events").expect("run");
        let error = event_page(run.clone(), vec![event(&run, 2), event(&run, 1)], None)
            .expect_err("non-monotonic stream");
        assert_eq!(error.class, ControlPlaneErrorClass::InvalidArgument);

        let foreign = RunId::new("foreign-run").expect("foreign run");
        let error = event_page(run, vec![event(&foreign, 1)], None).expect_err("foreign stream");
        assert_eq!(error.class, ControlPlaneErrorClass::InvalidArgument);
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
                ControlPlaneOperation::ListMissions,
                ControlPlaneOperation::GetMission,
                ControlPlaneOperation::SubmitRun,
                ControlPlaneOperation::ListRuns,
                ControlPlaneOperation::GetRun,
                ControlPlaneOperation::ListRunTasks,
                ControlPlaneOperation::GetRunTask,
                ControlPlaneOperation::ListTaskAttempts,
                ControlPlaneOperation::GetTaskAttempt,
                ControlPlaneOperation::ListAttemptExecutions,
                ControlPlaneOperation::GetAttemptExecution,
                ControlPlaneOperation::ListRunArtifacts,
                ControlPlaneOperation::GetRunArtifact,
                ControlPlaneOperation::RegisterRunArtifact,
                ControlPlaneOperation::ListRunEvidence,
                ControlPlaneOperation::GetRunEvidence,
                ControlPlaneOperation::RegisterRunEvidence,
                ControlPlaneOperation::ListRunExternalReferences,
                ControlPlaneOperation::GetRunExternalReference,
                ControlPlaneOperation::RegisterRunExternalReference,
                ControlPlaneOperation::GetRunEvents,
                ControlPlaneOperation::GetRunEventRetention,
                ControlPlaneOperation::AppendRunEvent,
                ControlPlaneOperation::GetRunReview,
                ControlPlaneOperation::ExecuteRunAction,
            ]
        );
        assert!(!capabilities.operations.is_empty());
        assert!(capabilities.compatibility_windows.is_empty());
    }

    #[test]
    fn run_discovery_is_stably_paginated() {
        let mut snapshots = BTreeMap::new();
        for (run_id, submitted_at) in [
            ("run-oldest", "2026-01-01T00:00:00Z"),
            ("run-middle", "2026-01-02T00:00:00Z"),
            ("run-newest", "2026-01-03T00:00:00Z"),
        ] {
            let mut snapshot = snapshot(run_id, None);
            snapshot.record.submitted_at = submitted_at.to_string();
            snapshots.insert(run_id.to_string(), snapshot);
        }
        let first_service = OrchestrationService::new(MapLookup {
            snapshots: snapshots.clone(),
        });
        let first = first_service
            .runs(&ControlPlaneRunListRequest {
                limit: 1,
                ..Default::default()
            })
            .expect("first page");
        assert_eq!(first.runs[0].run.as_str(), "run-newest");
        assert!(first.has_more);

        let mut inserted = snapshot("run-inserted", None);
        inserted.record.submitted_at = "2026-01-04T00:00:00Z".to_string();
        snapshots.insert("run-inserted".to_string(), inserted);
        let service = OrchestrationService::new(MapLookup { snapshots });
        let second = service
            .runs(&ControlPlaneRunListRequest {
                mission: None,
                cursor: first.next_cursor,
                limit: 1,
            })
            .expect("second page");
        assert_eq!(second.runs[0].run.as_str(), "run-middle");
        assert!(second.has_more);

        let third = service
            .runs(&ControlPlaneRunListRequest {
                mission: None,
                cursor: second.next_cursor,
                limit: 1,
            })
            .expect("third page");
        assert_eq!(third.runs[0].run.as_str(), "run-oldest");
        assert!(!third.has_more);
        assert!(third.next_cursor.is_none());
    }

    #[test]
    fn run_discovery_rejects_unknown_cursor_encodings() {
        let error = service()
            .runs(&ControlPlaneRunListRequest {
                mission: None,
                cursor: Some(RunCursor::new("not-a-run-cursor").expect("opaque cursor")),
                limit: 10,
            })
            .expect_err("invalid cursor");
        assert_eq!(error.class, ControlPlaneErrorClass::InvalidArgument);
    }

    #[test]
    fn task_discovery_is_run_scoped_bounded_and_cursor_bound() {
        let mut run_snapshot = snapshot("run-with-tasks", None);
        run_snapshot.record.tasks = [
            ("z-task", AgentTaskState::Succeeded),
            ("a-task", AgentTaskState::Running),
            ("m-task", AgentTaskState::Blocked),
        ]
        .into_iter()
        .map(|(task_id, state)| AgentTaskRunTask {
            task_id: task_id.to_string(),
            state,
            backend: "fixture".to_string(),
            selector: None,
            model: None,
            provider_ref: None,
        })
        .collect();
        run_snapshot.record.metadata["provider_executions"] = json!([
            {
                "key": "m-task:1",
                "task_id": "m-task",
                "attempt": 1,
                "state": "failed",
                "started_at": "2026-01-01T00:00:00Z",
                "finished_at": "2026-01-01T00:01:00Z",
                "owner_identity": "run-with-tasks:m-task:1"
            },
            {
                "key": "m-task:2",
                "task_id": "m-task",
                "attempt": 2,
                "state": "running",
                "started_at": "2026-01-01T00:02:00Z",
                "owner_identity": "run-with-tasks:m-task:2",
                "execution_identity": "run-with-tasks:m-task:2:execution"
            }
        ]);
        let service = OrchestrationService::new(MapLookup {
            snapshots: BTreeMap::from([("run-with-tasks".to_string(), run_snapshot)]),
        });
        let run = RunId::new("run-with-tasks").expect("run");
        let first = service
            .tasks(
                &run,
                &ControlPlaneTaskListRequest {
                    limit: 2,
                    ..Default::default()
                },
            )
            .expect("first task page");
        assert_eq!(
            first
                .tasks
                .iter()
                .map(|task| task.task.as_str())
                .collect::<Vec<_>>(),
            vec!["a-task", "m-task"]
        );
        assert!(first.has_more);
        let detail = service
            .task(&run, &TaskId::new("m-task").expect("task"))
            .expect("task detail");
        assert_eq!(detail.state, ControlPlaneState::Blocked);
        let second = service
            .tasks(
                &run,
                &ControlPlaneTaskListRequest {
                    cursor: first.next_cursor.clone(),
                    limit: 2,
                },
            )
            .expect("second task page");
        assert_eq!(second.tasks[0].task.as_str(), "z-task");
        assert!(!second.has_more);
        let attempts = service
            .attempts(
                &run,
                &TaskId::new("m-task").expect("task"),
                &ControlPlaneAttemptListRequest {
                    limit: 1,
                    ..Default::default()
                },
            )
            .expect("attempt page");
        assert_eq!(attempts.attempts[0].attempt_number, 1);
        assert!(attempts.has_more);
        let active = service
            .attempt(&run, &TaskId::new("m-task").expect("task"), 2)
            .expect("active attempt");
        assert_eq!(active.state, ControlPlaneState::Running);
        assert_eq!(
            active.execution.as_ref().map(ExecutionId::as_str),
            Some("run-with-tasks:m-task:2:execution")
        );
        let executions = service
            .executions(&run, &TaskId::new("m-task").expect("task"), 2)
            .expect("execution page");
        assert_eq!(executions.executions.len(), 1);
        let execution = service
            .execution(
                &run,
                &TaskId::new("m-task").expect("task"),
                2,
                &ExecutionId::new("run-with-tasks:m-task:2:execution").expect("execution"),
            )
            .expect("execution detail");
        assert_eq!(execution.attempt.as_str(), "run-with-tasks:m-task:2");
        let error = service
            .tasks(
                &RunId::new("another-run").expect("run"),
                &ControlPlaneTaskListRequest {
                    cursor: first.next_cursor,
                    limit: 2,
                },
            )
            .expect_err("cursor is bound to its run");
        assert_eq!(error.class, ControlPlaneErrorClass::InvalidArgument);

        let mut duplicate = snapshot("run-with-duplicates", None);
        duplicate.record.tasks = vec![
            AgentTaskRunTask {
                task_id: "same-task".to_string(),
                state: AgentTaskState::Running,
                backend: "fixture".to_string(),
                selector: None,
                model: None,
                provider_ref: None,
            },
            AgentTaskRunTask {
                task_id: "same-task".to_string(),
                state: AgentTaskState::Succeeded,
                backend: "fixture".to_string(),
                selector: None,
                model: None,
                provider_ref: None,
            },
        ];
        let duplicate_run = RunId::new("run-with-duplicates").expect("run");
        let duplicate_service = OrchestrationService::new(MapLookup {
            snapshots: BTreeMap::from([("run-with-duplicates".to_string(), duplicate)]),
        });
        let error = duplicate_service
            .tasks(&duplicate_run, &ControlPlaneTaskListRequest::default())
            .expect_err("duplicate task identities fail closed");
        assert_eq!(error.class, ControlPlaneErrorClass::InvalidArgument);
    }

    #[test]
    fn reference_registration_is_durable_idempotent_and_run_scoped() {
        with_isolated_home(|_| {
            let store = AgentTaskLifecycleStore::from_current_environment().expect("store");
            store.write_record(&record(AGENT_TASK_RUN)).expect("record");
            let run = RunId::new(AGENT_TASK_RUN).expect("run");
            let request = ControlPlaneReferenceRegistration {
                schema: CONTROL_PLANE_REFERENCE_REGISTRATION_SCHEMA.to_string(),
                idempotency_key: "external-runner-job-1".to_string(),
                actor: "broker:controller".to_string(),
                reference: ReferenceId::new("runner-job-1").expect("reference"),
                kind: "runner_job".to_string(),
                uri: "homeboy://runner/jobs/1?token=secret-token#fragment-secret".to_string(),
            };
            let first = register_reference_in_store(
                &store,
                &run,
                ControlPlaneReferenceType::ExternalReference,
                &request,
            )
            .expect("registration");
            let replay = register_reference_in_store(
                &store,
                &run,
                ControlPlaneReferenceType::ExternalReference,
                &request,
            )
            .expect("idempotent replay");
            assert_eq!(first, replay);
            assert!(!first.uri.contains("secret-token"));
            assert!(!first.uri.contains("fragment-secret"));
            let persisted = serde_json::to_string(
                &store.read_record(AGENT_TASK_RUN).expect("persisted record"),
            )
            .expect("serialize persisted record");
            assert!(!persisted.contains("external-runner-job-1"));
            assert!(!persisted.contains("secret-token"));
            assert!(!persisted.contains("fragment-secret"));
            let service = OrchestrationService::new(LifecycleStoreLookup::new(store));
            let page = service
                .references(&run, ControlPlaneReferenceType::ExternalReference)
                .expect("reference page");
            assert_eq!(page.references, vec![first.clone()]);
            assert_eq!(
                service
                    .reference(
                        &run,
                        ControlPlaneReferenceType::ExternalReference,
                        &request.reference,
                    )
                    .expect("reference detail"),
                first
            );
            let mut conflicting = request;
            conflicting.uri = "homeboy://runner/jobs/2".to_string();
            let error = service
                .register_reference(
                    &run,
                    ControlPlaneReferenceType::ExternalReference,
                    &conflicting,
                )
                .expect_err("conflicting idempotency key");
            assert_eq!(error.class, ControlPlaneErrorClass::InvalidArgument);
        });
    }

    #[test]
    fn automatic_references_have_stable_unique_identities_and_safe_uris() {
        let mut record = record(AGENT_TASK_RUN);
        record.artifact_refs = vec![
            AgentTaskArtifactRef {
                task_id: "task-with-unlabelled-artifacts".to_string(),
                kind: "patch".to_string(),
                uri: "https://example.test/first?token=secret#fragment".to_string(),
                role: None,
                label: None,
                semantic_key: None,
                size_bytes: None,
            },
            AgentTaskArtifactRef {
                task_id: "task-with-unlabelled-artifacts".to_string(),
                kind: "patch".to_string(),
                uri: "https://example.test/second".to_string(),
                role: None,
                label: None,
                semantic_key: None,
                size_bytes: None,
            },
            AgentTaskArtifactRef {
                task_id: "task-with-unlabelled-artifacts".to_string(),
                kind: "patch".to_string(),
                uri: "https://example.test/first?token=another-secret".to_string(),
                role: None,
                label: None,
                semantic_key: None,
                size_bytes: None,
            },
        ];

        let references = references_for_record(&record, ControlPlaneReferenceType::Artifact)
            .expect("automatic references");

        assert_eq!(references.len(), 2);
        assert_ne!(references[0].reference, references[1].reference);
        assert!(references
            .iter()
            .all(|reference| !reference.uri.contains("secret")));
        assert!(references
            .iter()
            .all(|reference| !reference.uri.contains('#')));
    }

    #[test]
    fn malformed_durable_reference_registry_fails_closed() {
        let mut record = record(AGENT_TASK_RUN);
        record.metadata["control_plane_references"] = json!({ "unexpected": true });

        let error = references_for_record(&record, ControlPlaneReferenceType::ExternalReference)
            .expect_err("malformed registry");

        assert_eq!(error.class, ControlPlaneErrorClass::InvalidArgument);

        record.metadata["control_plane_references"] = json!([{
            "reference_type": "external_reference",
            "reference": "runner?token=secret",
            "kind": "runner_job",
            "uri": "homeboy://runner/jobs/1",
            "registered_at": "2026-01-01T00:00:00Z",
            "actor": "broker:controller",
            "idempotency_digest": "a".repeat(64),
        }]);
        references_for_record(&record, ControlPlaneReferenceType::ExternalReference)
            .expect_err("unsafe persisted identity");

        record.metadata["control_plane_references"] = Value::Array(
            (0..=super::REGISTERED_REFERENCE_BOUND)
                .map(|index| {
                    json!({
                        "reference_type": "artifact",
                        "reference": format!("artifact-{index}"),
                        "kind": "patch",
                        "uri": format!("homeboy://artifact/{index}"),
                        "registered_at": "2026-01-01T00:00:00Z",
                        "actor": "broker:controller",
                        "idempotency_digest": "a".repeat(64),
                    })
                })
                .collect(),
        );
        let error = references_for_record(&record, ControlPlaneReferenceType::Artifact)
            .expect_err("over-bound registry");
        assert_eq!(error.class, ControlPlaneErrorClass::InvalidArgument);
    }

    #[test]
    fn mission_cursor_round_trips_and_rejects_unknown_encodings() {
        let position = homeboy_core::observation::MissionCursor {
            created_at: "2026-01-01T00:00:00Z".to_string(),
            id: "mission-1".to_string(),
        };
        let cursor = encode_mission_cursor(&position).expect("encode cursor");
        assert_eq!(
            decode_mission_cursor(&cursor).expect("decode cursor"),
            position
        );
        let error = decode_mission_cursor(
            &homeboy_control_plane_contract::MissionCursor::new("not-a-mission-cursor")
                .expect("opaque cursor"),
        )
        .expect_err("invalid cursor");
        assert_eq!(error.class, ControlPlaneErrorClass::InvalidArgument);
    }

    #[test]
    fn lifecycle_store_run_discovery_uses_bounded_keyset_pages() {
        with_isolated_home(|_| {
            let store = AgentTaskLifecycleStore::from_current_environment().expect("store");
            for (run_id, submitted_at) in [
                ("run-oldest", "2026-01-01T00:00:00Z"),
                ("run-middle", "2026-01-02T00:00:00Z"),
                ("run-newest", "2026-01-03T00:00:00Z"),
            ] {
                let mut record = record(run_id);
                record.submitted_at = submitted_at.to_string();
                record.metadata = json!({
                    "fanout": {
                        "id": if run_id == "run-oldest" {
                            "mission-b"
                        } else {
                            "mission-a"
                        }
                    }
                });
                store.write_record(&record).expect("record");
            }
            let service = OrchestrationService::new(LifecycleStoreLookup::new(store));

            let first = service
                .runs(&ControlPlaneRunListRequest {
                    limit: 2,
                    ..Default::default()
                })
                .expect("first page");
            assert_eq!(
                first
                    .runs
                    .iter()
                    .map(|run| run.run.as_str())
                    .collect::<Vec<_>>(),
                vec!["run-newest", "run-middle"]
            );
            assert!(first.has_more);

            let second = service
                .runs(&ControlPlaneRunListRequest {
                    mission: None,
                    cursor: first.next_cursor,
                    limit: 2,
                })
                .expect("second page");
            assert_eq!(second.runs[0].run.as_str(), "run-oldest");
            assert!(!second.has_more);
            assert!(second.next_cursor.is_none());

            let mission_a = MissionId::new("mission-a").expect("mission");
            let filtered_first = service
                .runs(&ControlPlaneRunListRequest {
                    mission: Some(mission_a.clone()),
                    limit: 1,
                    ..Default::default()
                })
                .expect("first filtered page");
            assert_eq!(filtered_first.runs[0].run.as_str(), "run-newest");
            assert!(filtered_first.has_more);
            let filtered_second = service
                .runs(&ControlPlaneRunListRequest {
                    mission: Some(mission_a),
                    cursor: filtered_first.next_cursor.clone(),
                    limit: 1,
                })
                .expect("second filtered page");
            assert_eq!(filtered_second.runs[0].run.as_str(), "run-middle");
            assert!(!filtered_second.has_more);
            let mismatch = service
                .runs(&ControlPlaneRunListRequest {
                    mission: Some(MissionId::new("mission-b").expect("mission")),
                    cursor: filtered_first.next_cursor,
                    limit: 1,
                })
                .expect_err("cursor is bound to its mission filter");
            assert_eq!(mismatch.class, ControlPlaneErrorClass::InvalidArgument);
        });
    }

    #[test]
    fn registered_provider_submits_a_prepared_controller_plan_once() {
        with_isolated_home(|_| {
            let run_id = "prepared-control-plane-run";
            let store = AgentTaskLifecycleStore::from_current_environment().expect("store");
            let mut plan = AgentTaskPlan::new("prepared-plan", Vec::new());
            plan.metadata = json!({
                "fanout": {
                    "id": "prepared-fanout-mission",
                    "plane": "isolated_tasks"
                }
            });
            store
                .write_controller_plan(run_id, &plan)
                .expect("prepared plan");
            let request = ControlPlaneSubmissionRequest {
                schema: CONTROL_PLANE_SUBMISSION_REQUEST_SCHEMA.to_string(),
                idempotency_key: run_id.to_string(),
                actor: "control-plane-test".to_string(),
                run: RunId::new(run_id).expect("run"),
                queue_only: true,
            };

            let first = RegisteredProvider.submit(&request).expect("submit");
            let replay = RegisteredProvider.submit(&request).expect("replay");
            assert_eq!(first.acknowledgement, replay.acknowledgement);
            assert_eq!(first.run.as_str(), run_id);
            assert!(first.queued);
            assert_eq!(first.outcome, ControlPlaneActionOutcome::Succeeded);
            assert_eq!(replay.outcome, ControlPlaneActionOutcome::Succeeded);
            assert!(store.record_exists(run_id).expect("record exists"));
            assert_eq!(
                first
                    .resource
                    .mission
                    .as_ref()
                    .map(|mission| mission.as_str()),
                Some("prepared-fanout-mission")
            );
            let mission = RegisteredProvider
                .mission(&MissionId::new("prepared-fanout-mission").expect("mission"))
                .expect("mission detail");
            assert_eq!(mission.run_count, 1);
            let missions = RegisteredProvider
                .missions(&ControlPlaneMissionListRequest {
                    limit: 1,
                    ..Default::default()
                })
                .expect("mission page");
            assert_eq!(missions.missions, vec![mission]);
            assert!(!missions.has_more);
            assert_eq!(
                store
                    .read_record(run_id)
                    .expect("record")
                    .metadata
                    .pointer("/fanout/id")
                    .and_then(Value::as_str),
                Some("prepared-fanout-mission")
            );

            for invalid_run in ["prepared/control-plane-run", &"x".repeat(257)] {
                let invalid = ControlPlaneSubmissionRequest {
                    schema: CONTROL_PLANE_SUBMISSION_REQUEST_SCHEMA.to_string(),
                    idempotency_key: invalid_run.to_string(),
                    actor: "control-plane-test".to_string(),
                    run: RunId::new(invalid_run).expect("opaque run"),
                    queue_only: true,
                };
                let error = RegisteredProvider
                    .submit(&invalid)
                    .expect_err("unsafe staged-plan identity");
                assert_eq!(error.class, ControlPlaneErrorClass::InvalidArgument);
            }

            let malformed_run = "malformed-fanout-run";
            let mut malformed_plan = AgentTaskPlan::new("malformed-fanout-plan", Vec::new());
            malformed_plan.metadata = json!({ "fanout": { "id": AGENT_TASK_RUN } });
            store
                .write_controller_plan(malformed_run, &malformed_plan)
                .expect("staged malformed plan");
            let malformed_request = ControlPlaneSubmissionRequest {
                schema: CONTROL_PLANE_SUBMISSION_REQUEST_SCHEMA.to_string(),
                idempotency_key: malformed_run.to_string(),
                actor: "control-plane-test".to_string(),
                run: RunId::new(malformed_run).expect("run"),
                queue_only: true,
            };
            RegisteredProvider
                .submit(&malformed_request)
                .expect_err("run-shaped fanout cannot be persisted as a mission");
            assert!(!store.record_exists(malformed_run).expect("record absent"));
        });
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
            let durable_events = service.events(&run, None).expect("durable action events");
            assert_eq!(
                durable_events
                    .events
                    .iter()
                    .map(|event| event.kind.as_str())
                    .collect::<Vec<_>>(),
                vec!["action.accepted", "action.already_satisfied"]
            );
            assert_eq!(durable_events.events[0].data["actor"], "test");
            assert_eq!(durable_events.events[0].data["confirmed"], true);
            assert_eq!(
                durable_events.events[0].data["expected_updated_at"],
                "2026-01-01T00:01:00Z"
            );
            assert_eq!(
                durable_events.events[0].data["acknowledgement"],
                first.acknowledgement
            );
            let legacy_logs =
                crate::agent_task_lifecycle::logs_in_store(&service.lookup.store, run.as_str())
                    .expect("legacy synthesized logs");
            let action_kinds: Vec<_> = legacy_logs
                .events
                .iter()
                .filter(|event| event.kind.starts_with("action."))
                .map(|event| event.kind.as_str())
                .collect();
            assert!(
                action_kinds.is_empty(),
                "ledger-backed claims are not synthesized"
            );
            let mut conflicting = request;
            conflicting.parameters.data = json!({ "reason": "different reason" });
            let error = service
                .execute_action(&run, &conflicting)
                .expect_err("conflicting key");
            assert_eq!(error.class, ControlPlaneErrorClass::InvalidArgument);
            assert_eq!(
                service
                    .events(&run, None)
                    .expect("conflict does not append events")
                    .events
                    .len(),
                2
            );

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
            assert_eq!(
                service
                    .events(&run, None)
                    .expect("failed action events")
                    .events
                    .iter()
                    .filter(|event| event.kind == "action.failed")
                    .count(),
                1
            );

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
            assert_eq!(
                service
                    .events(&run, None)
                    .expect("reconcile action events")
                    .events
                    .iter()
                    .filter(|event| event.kind == "action.already_satisfied")
                    .count(),
                2,
                "the terminal cancel and reconcile each have one terminal receipt"
            );
            for index in 0..100 {
                let mut filler = event_append_request();
                filler.idempotency_key = format!("retention-filler-{index}");
                filler.kind = "run.progress".to_string();
                super::append_event_in_store(&service.lookup.store, &run, &filler)
                    .expect("append retention filler");
            }
            assert!(
                crate::agent_task_lifecycle::logs_in_store(&service.lookup.store, run.as_str(),)
                    .expect("logs after retention")
                    .events
                    .iter()
                    .all(|event| !event.kind.starts_with("action.")),
                "compact receipts suppress legacy action synthesis after payload retention"
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
    fn interrupted_cancel_recovers_terminal_evidence_without_repeating_effects() {
        with_isolated_home(|_| {
            let store = AgentTaskLifecycleStore::from_current_environment().expect("store");
            crate::agent_task_lifecycle::submit_plan_in_store(
                &store,
                &AgentTaskPlan::new("interrupted-cancel", Vec::new()),
                Some(AGENT_TASK_RUN),
            )
            .expect("queued record");
            let service = OrchestrationService::new(LifecycleStoreLookup::new(store.clone()));
            let run = RunId::new(AGENT_TASK_RUN).expect("run");
            let request = ControlPlaneActionRequest {
                schema: CONTROL_PLANE_ACTION_REQUEST_SCHEMA.to_string(),
                action: ControlPlaneAction::Cancel,
                idempotency_key: "interrupted-cancel-1".to_string(),
                actor: "test".to_string(),
                expected_updated_at: None,
                parameters: ControlPlaneActionPayload {
                    schema: CONTROL_PLANE_CANCEL_PARAMETERS_SCHEMA.to_string(),
                    data: json!({ "reason": "stop" }),
                },
                confirmed: true,
            };
            let operation_key = interrupt_action_after_effect(&service, &run, &request, || {
                crate::agent_task_lifecycle::cancel_run_in_store(
                    &store,
                    run.as_str(),
                    Some("stop"),
                )
                .expect("cancel effect");
            });

            let recovered = service
                .execute_action_with_delegates(
                    &run,
                    &request,
                    |_| panic!("retry delegate must not run"),
                    || panic!("resume delegate must not run"),
                    |_| panic!("promote delegate must not run"),
                )
                .expect("recover interrupted cancel");
            assert_eq!(recovered.outcome, ControlPlaneActionOutcome::Succeeded);
            assert_eq!(recovered.resource.state, ControlPlaneRunState::Cancelled);
            assert_eq!(
                operation_claim_in_store(&store, run.as_str(), &operation_key)
                    .expect("claim")
                    .expect("claim exists")
                    .state,
                ClaimState::Completed
            );
            assert_eq!(
                service
                    .events(&run, None)
                    .expect("action receipts")
                    .events
                    .iter()
                    .map(|event| event.kind.as_str())
                    .collect::<Vec<_>>(),
                vec!["action.accepted", "action.succeeded"]
            );
            assert_eq!(
                service.execute_action(&run, &request).expect("replay"),
                recovered
            );
        });
    }

    #[test]
    fn interrupted_ambiguous_resume_terminalizes_without_redispatch() {
        with_isolated_home(|_| {
            let store = AgentTaskLifecycleStore::from_current_environment().expect("store");
            crate::agent_task_lifecycle::submit_plan_in_store(
                &store,
                &AgentTaskPlan::new("interrupted-resume", Vec::new()),
                Some(AGENT_TASK_RUN),
            )
            .expect("queued record");
            let service = OrchestrationService::new(LifecycleStoreLookup::new(store.clone()));
            let run = RunId::new(AGENT_TASK_RUN).expect("run");
            let request = ControlPlaneActionRequest {
                schema: CONTROL_PLANE_ACTION_REQUEST_SCHEMA.to_string(),
                action: ControlPlaneAction::Resume,
                idempotency_key: "interrupted-resume-1".to_string(),
                actor: "test".to_string(),
                expected_updated_at: None,
                parameters: ControlPlaneActionPayload::empty(),
                confirmed: false,
            };
            let effect_count = Cell::new(0);
            let operation_key = interrupt_action_after_effect(&service, &run, &request, || {
                effect_count.set(effect_count.get() + 1);
            });
            let accepted_at = operation_claim_in_store(&store, run.as_str(), &operation_key)
                .expect("claim")
                .expect("claim exists")
                .accepted_at
                .expect("accepted timestamp");
            for index in 0..100 {
                let mut filler = event_append_request();
                filler.idempotency_key = format!("interrupted-resume-filler-{index}");
                filler.kind = "run.progress".to_string();
                super::append_event_in_store(&store, &run, &filler).expect("retention filler");
            }
            assert!(service
                .events(&run, None)
                .expect("compacted events")
                .events
                .iter()
                .all(|event| event.kind != "action.accepted"));

            let recovered = service
                .execute_action_with_delegates(
                    &run,
                    &request,
                    |_| panic!("retry delegate must not run"),
                    || {
                        effect_count.set(effect_count.get() + 1);
                        panic!("resume delegate must not run")
                    },
                    |_| panic!("promote delegate must not run"),
                )
                .expect("terminal interrupted resume acknowledgement");
            assert_eq!(effect_count.get(), 1);
            assert_eq!(recovered.outcome, ControlPlaneActionOutcome::Failed);
            assert_eq!(recovered.accepted_at, accepted_at);
            assert!(recovered
                .message
                .as_deref()
                .is_some_and(|message| message.contains("no second execution")));
            assert_eq!(
                operation_claim_in_store(&store, run.as_str(), &operation_key)
                    .expect("claim")
                    .expect("claim exists")
                    .state,
                ClaimState::Completed
            );
            assert_eq!(
                service
                    .events(&run, None)
                    .expect("action receipts")
                    .events
                    .iter()
                    .filter(|event| event.kind.starts_with("action."))
                    .map(|event| event.kind.as_str())
                    .collect::<Vec<_>>(),
                vec!["action.failed"]
            );
            assert_eq!(
                service.execute_action(&run, &request).expect("replay"),
                recovered
            );
        });
    }

    #[test]
    fn interrupted_retry_recovers_its_deterministic_successor_without_redispatch() {
        with_isolated_home(|_| {
            let store = AgentTaskLifecycleStore::from_current_environment().expect("store");
            crate::agent_task_lifecycle::submit_plan_in_store(
                &store,
                &AgentTaskPlan::new("interrupted-retry", Vec::new()),
                Some(AGENT_TASK_RUN),
            )
            .expect("source record");
            let service = OrchestrationService::new(LifecycleStoreLookup::new(store.clone()));
            let run = RunId::new(AGENT_TASK_RUN).expect("run");
            let retry_run_id = "interrupted-retry-successor";
            let request = ControlPlaneActionRequest {
                schema: CONTROL_PLANE_ACTION_REQUEST_SCHEMA.to_string(),
                action: ControlPlaneAction::Retry,
                idempotency_key: "interrupted-retry-1".to_string(),
                actor: "test".to_string(),
                expected_updated_at: None,
                parameters: ControlPlaneActionPayload {
                    schema: CONTROL_PLANE_RETRY_PARAMETERS_SCHEMA.to_string(),
                    data: json!({
                        "new_run_id": retry_run_id,
                        "force": false,
                    }),
                },
                confirmed: true,
            };
            let operation_key = interrupt_action_after_effect(&service, &run, &request, || {
                let mut successor = record(retry_run_id);
                successor.metadata["retry_of"] = json!(AGENT_TASK_RUN);
                store.write_record(&successor).expect("durable successor");
            });

            let recovered = service
                .execute_action_with_delegates(
                    &run,
                    &request,
                    |_| panic!("retry delegate must not run"),
                    || panic!("resume delegate must not run"),
                    |_| panic!("promote delegate must not run"),
                )
                .expect("recover interrupted retry");
            assert_eq!(
                recovered.outcome,
                ControlPlaneActionOutcome::AlreadySatisfied
            );
            assert_eq!(recovered.result.schema, CONTROL_PLANE_RETRY_RESULT_SCHEMA);
            assert_eq!(recovered.result.data["record"]["run_id"], retry_run_id);
            assert_eq!(recovered.result.data["recovered"], true);
            assert_eq!(
                operation_claim_in_store(&store, run.as_str(), &operation_key)
                    .expect("claim")
                    .expect("claim exists")
                    .state,
                ClaimState::Completed
            );
            assert_eq!(
                service.execute_action(&run, &request).expect("replay"),
                recovered
            );
        });
    }

    #[test]
    fn interrupted_promotion_recovers_only_its_exact_request_report() {
        with_isolated_home(|_| {
            let store = AgentTaskLifecycleStore::from_current_environment().expect("store");
            store.write_record(&record(AGENT_TASK_RUN)).expect("record");
            let service = OrchestrationService::new(LifecycleStoreLookup::new(store.clone()));
            let run = RunId::new(AGENT_TASK_RUN).expect("run");
            let request = ControlPlaneActionRequest {
                schema: CONTROL_PLANE_ACTION_REQUEST_SCHEMA.to_string(),
                action: ControlPlaneAction::Promote,
                idempotency_key: "interrupted-promote-1".to_string(),
                actor: "test".to_string(),
                expected_updated_at: None,
                parameters: ControlPlaneActionPayload {
                    schema: CONTROL_PLANE_PROMOTE_PARAMETERS_SCHEMA.to_string(),
                    data: json!({
                        "source": "{}",
                        "source_run_id": AGENT_TASK_RUN,
                        "to_worktree": "homeboy@candidate",
                        "artifact_id": "patch-1",
                        "dry_run": false,
                    }),
                },
                confirmed: true,
            };
            let parameters: crate::agent_task_service::AgentTaskPromotionRequest =
                serde_json::from_value(request.parameters.data.clone()).expect("parameters");
            let fingerprint = crate::agent_task_service::promotion_request_fingerprint(&parameters)
                .expect("request fingerprint");
            let mut report: crate::agent_task_promotion::AgentTaskPromotionReport =
                serde_json::from_value(json!({
                    "schema": "homeboy/agent-task-promotion-report/v1",
                    "status": "applied",
                    "source": { "kind": "aggregate", "run_id": AGENT_TASK_RUN, "task_id": "task" },
                    "to_worktree": "homeboy@candidate",
                    "target": { "worktree": "homeboy@candidate" },
                    "patch_artifact": { "id": "patch-1", "kind": "patch", "path": "patch" },
                    "operator_notification": { "status": "completed", "message": "complete" }
                }))
                .expect("promotion report");
            report.provenance["promotion_request_fingerprint"] = json!(fingerprint);
            interrupt_action_after_effect(&service, &run, &request, || {
                store
                    .record_promotion(
                        run.as_str(),
                        serde_json::to_value(report).expect("report value"),
                    )
                    .expect("durable promotion report");
            });

            let recovered = service
                .execute_action_with_delegates(
                    &run,
                    &request,
                    |_| panic!("retry delegate must not run"),
                    || panic!("resume delegate must not run"),
                    |_| panic!("promote delegate must not run"),
                )
                .expect("recover interrupted promotion");
            assert_eq!(recovered.outcome, ControlPlaneActionOutcome::Succeeded);
            assert_eq!(recovered.result.schema, CONTROL_PLANE_PROMOTE_RESULT_SCHEMA);
            assert_eq!(
                recovered.result.data["handoff"]["states"]["target_applied"],
                true
            );
            assert_eq!(
                service.execute_action(&run, &request).expect("replay"),
                recovered
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
    fn action_audit_supports_the_maximum_action_idempotency_key() {
        with_isolated_home(|_| {
            let store = AgentTaskLifecycleStore::from_current_environment().expect("store");
            store.write_record(&record(AGENT_TASK_RUN)).expect("record");
            let service = OrchestrationService::new(LifecycleStoreLookup::new(store));
            let run = RunId::new(AGENT_TASK_RUN).expect("run");
            let request = ControlPlaneActionRequest {
                schema: CONTROL_PLANE_ACTION_REQUEST_SCHEMA.to_string(),
                action: ControlPlaneAction::Reconcile,
                idempotency_key: "k".repeat(128),
                actor: "test".to_string(),
                expected_updated_at: None,
                parameters: ControlPlaneActionPayload::empty(),
                confirmed: true,
            };

            let acknowledgement = service.execute_action(&run, &request).expect("action");
            assert_eq!(
                service.execute_action(&run, &request).expect("replay"),
                acknowledgement
            );
            assert_eq!(service.events(&run, None).expect("events").events.len(), 2);
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
    fn placement_projects_a_controller_decision_without_runner_inference() {
        let mut record = record(AGENT_TASK_RUN);
        let decision = homeboy_lab_runner_contract::ExecutionPlacementDecision::controller_local(
            "route",
            "1",
            homeboy_lab_runner_contract::ExecutionPlacementIdentity {
                repository: "repo".to_string(),
                workspace: "workspace".to_string(),
                task: "task".to_string(),
                candidate: None,
                base: None,
            },
            homeboy_lab_runner_contract::Placement::Local,
        );
        record.metadata["execution_placement_decision"] = serde_json::to_value(decision).unwrap();

        let value = serde_json::to_value(project_record(&record, None).unwrap()).unwrap();
        assert_eq!(value["placement"]["requested"], "controller");
        assert_eq!(value["placement"]["selected"], "controller");
        assert!(value["placement"].get("runner_id").is_none());
        assert!(value["placement"].get("effective").is_none());
    }

    #[test]
    fn placement_projects_a_correlated_runner_outcome() {
        let mut record = record(AGENT_TASK_RUN);
        let decision =
            runner_placement_decision(homeboy_lab_runner_contract::Placement::Lab, false);
        let outcome = decision
            .outcome(
                homeboy_lab_runner_contract::EffectiveExecutionPlacement::Lab,
                Some("runner-1".to_string()),
            )
            .unwrap();
        record.metadata["execution_placement_decision"] = serde_json::to_value(decision).unwrap();
        record.metadata["execution_placement_outcome"] = serde_json::to_value(outcome).unwrap();

        let value = serde_json::to_value(project_record(&record, None).unwrap()).unwrap();
        assert_eq!(value["placement"]["requested"], "runner");
        assert_eq!(value["placement"]["selected"], "runner");
        assert_eq!(value["placement"]["effective"], "runner");
        assert_eq!(value["placement"]["runner_id"], "runner-1");
    }

    #[test]
    fn placement_projects_a_verified_controller_fallback() {
        let mut record = record(AGENT_TASK_RUN);
        let decision =
            runner_placement_decision(homeboy_lab_runner_contract::Placement::LabOrLocal, true);
        let outcome = decision
            .outcome(
                homeboy_lab_runner_contract::EffectiveExecutionPlacement::Local,
                None,
            )
            .unwrap();
        record.metadata["execution_placement_decision"] = serde_json::to_value(decision).unwrap();
        record.metadata["execution_placement_outcome"] = serde_json::to_value(outcome).unwrap();

        let value = serde_json::to_value(project_record(&record, None).unwrap()).unwrap();
        assert_eq!(value["placement"]["selected"], "runner");
        assert_eq!(value["placement"]["effective"], "controller");
    }

    #[test]
    fn placement_omits_an_outcome_with_a_mismatched_decision_id() {
        let mut record = record(AGENT_TASK_RUN);
        let decision =
            runner_placement_decision(homeboy_lab_runner_contract::Placement::Lab, false);
        let mut outcome = decision
            .outcome(
                homeboy_lab_runner_contract::EffectiveExecutionPlacement::Lab,
                Some("runner-1".to_string()),
            )
            .unwrap();
        outcome.decision_id = "other-decision".to_string();
        record.metadata["execution_placement_decision"] = serde_json::to_value(decision).unwrap();
        record.metadata["execution_placement_outcome"] = serde_json::to_value(outcome).unwrap();

        let value = serde_json::to_value(project_record(&record, None).unwrap()).unwrap();
        assert!(value["placement"].get("effective").is_none());
    }

    #[test]
    fn placement_is_not_inferred_from_runner_id() {
        let record = record(AGENT_TASK_RUN);
        assert!(project_record(&record, None).unwrap().placement.is_none());
    }

    #[test]
    fn placement_omits_a_decision_with_a_forged_content_identity() {
        let mut record = record(AGENT_TASK_RUN);
        let mut decision =
            runner_placement_decision(homeboy_lab_runner_contract::Placement::Lab, false);
        decision.decision_id = "forged-decision".to_string();
        record.metadata["execution_placement_decision"] = serde_json::to_value(decision).unwrap();

        assert!(project_record(&record, None).unwrap().placement.is_none());
    }

    #[test]
    fn placement_omits_an_invalid_local_decision() {
        let mut record = record(AGENT_TASK_RUN);
        let mut decision =
            runner_placement_decision(homeboy_lab_runner_contract::Placement::Lab, false);
        decision.selected = homeboy_lab_runner_contract::EffectiveExecutionPlacement::Local;
        decision.runner = None;
        record.metadata["execution_placement_decision"] = serde_json::to_value(decision).unwrap();

        assert!(project_record(&record, None).unwrap().placement.is_none());
    }

    #[test]
    fn fanout_identity_owns_the_canonical_child_run_mission() {
        let record = record(AGENT_TASK_RUN);
        let mut legacy_plan = AgentTaskPlan::new("fanout-plan", Vec::new());
        legacy_plan.metadata = json!({
            "fanout": {
                "id": "fanout-portfolio-1",
                "plane": "isolated_tasks"
            }
        });
        let legacy = project_record(&record, Some(&legacy_plan)).expect("unpersisted projection");
        assert_eq!(
            legacy.mission.as_ref().map(|mission| mission.as_str()),
            Some(AGENT_TASK_COOK)
        );

        let mut durable_record = record;
        durable_record.metadata["fanout"] = legacy_plan.metadata["fanout"].clone();
        let durable = project_record(&durable_record, None).expect("durable projection");
        assert_eq!(
            durable.mission.as_ref().map(|mission| mission.as_str()),
            Some("fanout-portfolio-1")
        );
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
    fn queued_unmaterialized_admission_projects_reason_retry_and_manual_rearm_presentation() {
        let mut record = record(AGENT_TASK_RUN);
        record.state = AgentTaskRunState::Queued;
        record.metadata["unmaterialized_cook_admission"] = json!({
            "schema": "homeboy/unmaterialized-cook-admission/v1",
            "state": "queued",
            "reason": "Lab admission predicate controller_version != job_command_binary_version failed token=secret-value",
            "admission_attempts": 3,
            "retry": {
                "policy": "bounded_exponential",
                "next_attempt_at": "2099-01-01T00:00:00Z",
                "max_attempts": 20,
            },
        });

        let resource = project_record(&record, None).expect("project admission");
        let blocker = resource.blocker.expect("admission blocker");
        assert_eq!(blocker.code.as_deref(), Some("queued"));
        assert_eq!(blocker.state.as_deref(), Some("queued"));
        assert_eq!(
            blocker.reason.as_deref(),
            Some("Lab admission predicate controller_version != job_command_binary_version failed token=[REDACTED]")
        );
        assert_eq!(
            blocker.message,
            "Lab admission predicate controller_version != job_command_binary_version failed token=[REDACTED]"
        );
        let retry = blocker.retry.expect("bounded retry");
        assert_eq!(retry.policy, "bounded_exponential");
        assert_eq!(retry.attempts, 3);
        assert_eq!(retry.max_attempts, 20);
        assert_eq!(
            retry.next_attempt_at.as_deref(),
            Some("2099-01-01T00:00:00+00:00")
        );
        assert_eq!(
            retry.disposition,
            ControlPlaneAdmissionRetryDisposition::AutomaticReconciliationScheduled
        );
        let resume = resource
            .action_eligibility
            .expect("action eligibility")
            .actions
            .into_iter()
            .find(|action| action.action == ControlPlaneAction::Resume)
            .expect("resume action");
        assert_eq!(
            resume.availability,
            ControlPlaneActionAvailability::Available
        );
        assert!(resume.reason.contains("explicitly re-arms"));
        assert!(resume.reason.contains("recommended next action"));
    }

    #[test]
    fn exhausted_unmaterialized_admission_projects_its_bounded_terminal_disposition() {
        let mut record = record(AGENT_TASK_RUN);
        record.state = AgentTaskRunState::Failed;
        record.metadata["unmaterialized_cook_admission"] = json!({
            "schema": "homeboy/unmaterialized-cook-admission/v1",
            "state": "exhausted",
            "reason": "bounded Lab admission retry budget exhausted",
            "admission_attempts": 20,
            "retry": {
                "policy": "bounded_exponential",
                "next_attempt_at": "2026-09-08T03:14:05.251948+00:00",
                "max_attempts": 20,
            },
        });

        let resource = project_record(&record, None).expect("project exhausted admission");
        let blocker = resource.blocker.expect("admission blocker");
        assert_eq!(blocker.code.as_deref(), Some("exhausted"));
        assert_eq!(
            blocker.reason.as_deref(),
            Some("bounded Lab admission retry budget exhausted")
        );
        assert_eq!(
            blocker.retry.expect("bounded retry").disposition,
            ControlPlaneAdmissionRetryDisposition::Exhausted
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

    #[test]
    fn release_deploy_mission_projects_distinct_runs_artifact_and_target_graph() {
        with_isolated_home(|_| {
            let store = homeboy_core::observation::ObservationStore::open_initialized()
                .expect("observation store");
            let mission = "release-mission-13697";
            let digest = "a".repeat(64);
            let artifact = json!({
                "id": format!("sha256-{digest}"),
                "kind": "release-package",
                "uri": format!("sha256:{digest}"),
            });
            for (run_id, kind, tasks) in [
                ("release-run-13697", "release", json!([])),
                (
                    "deploy-run-13697",
                    "deploy",
                    json!([
                        { "id": "target-a", "state": "succeeded" },
                        { "id": "target-b", "state": "failed" },
                    ]),
                ),
            ] {
                let metadata = json!({
                    "control_plane": {
                        "kind": kind,
                        "phase": "completed",
                        "tasks": tasks,
                        "artifacts": [artifact.clone()],
                    }
                });
                store
                    .start_run_with_id_in_mission(
                        homeboy_core::observation::NewRunRecord::builder(kind)
                            .metadata(metadata.clone())
                            .build(),
                        run_id.to_string(),
                        mission,
                    )
                    .expect("start canonical run");
                store
                    .finish_run(
                        run_id,
                        if kind == "release" {
                            homeboy_core::observation::RunStatus::Pass
                        } else {
                            homeboy_core::observation::RunStatus::Fail
                        },
                        Some(metadata),
                    )
                    .expect("finish canonical run");
            }

            let provider = RegisteredProvider;
            let page = provider
                .runs(&ControlPlaneRunListRequest {
                    mission: Some(MissionId::new(mission).expect("mission")),
                    limit: 10,
                    ..Default::default()
                })
                .expect("mission runs");
            assert_eq!(page.runs.len(), 2);
            assert_ne!(page.runs[0].run, page.runs[1].run);
            assert!(page.runs.iter().all(|run| {
                run.artifacts
                    .iter()
                    .any(|artifact| artifact.uri == format!("sha256:{digest}"))
            }));

            let deploy = RunId::new("deploy-run-13697").expect("deploy run");
            let tasks = provider
                .tasks(&deploy, &ControlPlaneTaskListRequest::default())
                .expect("deploy target tasks");
            assert_eq!(tasks.tasks.len(), 2);
            assert_eq!(tasks.tasks[0].task.as_str(), "target-a");
            assert_eq!(tasks.tasks[0].state, ControlPlaneState::Succeeded);
            assert_eq!(tasks.tasks[1].state, ControlPlaneState::Failed);
            let attempt = provider
                .attempt(&deploy, &tasks.tasks[0].task, 1)
                .expect("target attempt");
            let execution = provider
                .execution(
                    &deploy,
                    &tasks.tasks[0].task,
                    1,
                    attempt.execution.as_ref().expect("execution id"),
                )
                .expect("target execution");
            assert_eq!(execution.state, ControlPlaneState::Succeeded);
        });
    }
}
