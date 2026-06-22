use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::{Error, Result};

pub const DETERMINISTIC_LOOP_PLAN_SCHEMA: &str = "homeboy/deterministic-loop-plan/v1";
pub const DETERMINISTIC_LOOP_RUN_IDENTITY_SCHEMA: &str =
    "homeboy/deterministic-loop-run-identity/v1";
pub const DETERMINISTIC_FANOUT_PLAN_SCHEMA: &str = "homeboy/deterministic-fanout-plan/v1";
pub const DETERMINISTIC_FANOUT_RESULT_SCHEMA: &str = "homeboy/deterministic-fanout-result/v1";
pub const DETERMINISTIC_EVIDENCE_SNAPSHOT_SCHEMA: &str =
    "homeboy/deterministic-evidence-snapshot/v1";
pub const DETERMINISTIC_LOOP_SPEC_SCHEMA: &str = "homeboy/deterministic-loop-spec/v1";
pub const DETERMINISTIC_LOOP_STATE_SCHEMA: &str = "homeboy/deterministic-loop-state/v1";
pub const DETERMINISTIC_LOOP_EVENT_SCHEMA: &str = "homeboy/deterministic-loop-event/v1";
pub const DETERMINISTIC_LOOP_ARTIFACT_DECLARATION_SCHEMA: &str =
    "homeboy/deterministic-loop-artifact-declaration/v1";

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeterministicLoopStatus {
    #[default]
    Planned,
    Running,
    Waiting,
    Succeeded,
    Failed,
    Canceled,
    Skipped,
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeterministicLoopRunIdentity {
    #[serde(default = "run_identity_schema")]
    pub schema: String,
    pub loop_id: String,
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    #[serde(default)]
    pub attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeterministicLoopPlan {
    #[serde(default = "loop_plan_schema")]
    pub schema: String,
    pub identity: DeterministicLoopRunIdentity,
    #[serde(default)]
    pub status: DeterministicLoopStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fanouts: Vec<DeterministicFanoutPlan>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeterministicFanoutPlan {
    #[serde(default = "fanout_plan_schema")]
    pub schema: String,
    pub fanout_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub records: Vec<DeterministicFanoutPlanRecord>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeterministicFanoutPlanRecord {
    pub record_id: String,
    pub dedupe_key: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub input: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeterministicFanoutResultRecord {
    #[serde(default = "fanout_result_schema")]
    pub schema: String,
    pub fanout_id: String,
    pub record_id: String,
    pub status: DeterministicLoopStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<DeterministicEvidenceRef>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub output: Value,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub diagnostics: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeterministicEvidenceSnapshot {
    #[serde(default = "evidence_snapshot_schema")]
    pub schema: String,
    pub snapshot_id: String,
    pub identity: DeterministicLoopRunIdentity,
    pub status: DeterministicLoopStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fanout_results: Vec<DeterministicFanoutResultRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<DeterministicEvidenceRef>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeterministicEvidenceRef {
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeterministicLoopSpec {
    #[serde(default = "loop_spec_schema")]
    pub schema: String,
    pub loop_id: String,
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
    #[serde(default)]
    pub stop: DeterministicLoopStopCriteria,
    #[serde(default)]
    pub retry: DeterministicLoopRetryPolicy,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<DeterministicLoopArtifactDeclaration>,
    #[serde(default)]
    pub validation: DeterministicLoopValidationPolicy,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeterministicLoopStopCriteria {
    #[serde(
        default = "default_stop_statuses",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub statuses: Vec<DeterministicLoopStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeterministicLoopRetryPolicy {
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    #[serde(
        default = "default_retry_statuses",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub statuses: Vec<DeterministicLoopStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeterministicLoopArtifactDeclaration {
    #[serde(default = "artifact_declaration_schema")]
    pub schema: String,
    pub artifact_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub local_evidence_required: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeterministicLoopValidationPolicy {
    #[serde(default)]
    pub require_declared_artifacts: bool,
    #[serde(default)]
    pub require_local_evidence: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeterministicLoopState {
    #[serde(default = "loop_state_schema")]
    pub schema: String,
    pub identity: DeterministicLoopRunIdentity,
    pub status: DeterministicLoopStatus,
    #[serde(default)]
    pub iteration: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<DeterministicLoopEvent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_lineage: Vec<DeterministicEvidenceRef>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeterministicLoopEvent {
    #[serde(default = "loop_event_schema")]
    pub schema: String,
    pub sequence: u32,
    pub iteration: u32,
    pub event_type: String,
    pub status: DeterministicLoopStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<DeterministicEvidenceRef>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeterministicLoopReconcileResult {
    pub status: DeterministicLoopStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<DeterministicEvidenceRef>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DeterministicLoopRunOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_from: Option<DeterministicLoopState>,
}

pub trait DeterministicLoopHooks {
    type IterationPlan;
    type IterationOutput;

    fn materialize_iteration(
        &mut self,
        state: &DeterministicLoopState,
    ) -> Result<Self::IterationPlan>;

    fn execute_iteration(
        &mut self,
        state: &DeterministicLoopState,
        plan: Self::IterationPlan,
    ) -> Result<Self::IterationOutput>;

    fn reconcile_iteration(
        &mut self,
        state: &DeterministicLoopState,
        output: Self::IterationOutput,
    ) -> Result<DeterministicLoopReconcileResult>;
}

pub type DeterministicLoopFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + 'a>>;

pub trait AsyncDeterministicLoopHooks {
    type IterationPlan;
    type IterationOutput;

    fn materialize_iteration_async<'a>(
        &'a mut self,
        state: &'a DeterministicLoopState,
    ) -> DeterministicLoopFuture<'a, Self::IterationPlan>;

    fn execute_iteration_async<'a>(
        &'a mut self,
        state: &'a DeterministicLoopState,
        plan: Self::IterationPlan,
    ) -> DeterministicLoopFuture<'a, Self::IterationOutput>;

    fn reconcile_iteration_async<'a>(
        &'a mut self,
        state: &'a DeterministicLoopState,
        output: Self::IterationOutput,
    ) -> DeterministicLoopFuture<'a, DeterministicLoopReconcileResult>;
}

pub trait DeterministicLoopEventSink {
    fn record_checkpoint(&mut self, state: &DeterministicLoopState) -> Result<()>;
    fn record_event(&mut self, event: &DeterministicLoopEvent) -> Result<()>;
}

pub trait DeterministicLoopCancellation {
    fn is_canceled(&self, state: &DeterministicLoopState) -> Result<bool>;
}

#[derive(Debug, Default)]
pub struct NoopDeterministicLoopEventSink;

#[derive(Debug, Default)]
pub struct NeverCancelDeterministicLoop;

impl DeterministicLoopEventSink for NoopDeterministicLoopEventSink {
    fn record_checkpoint(&mut self, _state: &DeterministicLoopState) -> Result<()> {
        Ok(())
    }

    fn record_event(&mut self, _event: &DeterministicLoopEvent) -> Result<()> {
        Ok(())
    }
}

impl DeterministicLoopCancellation for NeverCancelDeterministicLoop {
    fn is_canceled(&self, _state: &DeterministicLoopState) -> Result<bool> {
        Ok(false)
    }
}

impl DeterministicLoopRunIdentity {
    pub fn new(loop_id: impl Into<String>, run_id: impl Into<String>) -> Self {
        Self {
            schema: DETERMINISTIC_LOOP_RUN_IDENTITY_SCHEMA.to_string(),
            loop_id: loop_id.into(),
            run_id: run_id.into(),
            task_id: None,
            group_key: None,
            attempt_id: None,
            parent_run_id: None,
            attempt: 0,
            seed: None,
        }
    }
}

impl DeterministicLoopPlan {
    pub fn new(identity: DeterministicLoopRunIdentity) -> Self {
        Self {
            schema: DETERMINISTIC_LOOP_PLAN_SCHEMA.to_string(),
            identity,
            status: DeterministicLoopStatus::Planned,
            fanouts: Vec::new(),
            metadata: Value::Null,
        }
    }
}

impl DeterministicFanoutPlan {
    pub fn new(fanout_id: impl Into<String>) -> Self {
        Self {
            schema: DETERMINISTIC_FANOUT_PLAN_SCHEMA.to_string(),
            fanout_id: fanout_id.into(),
            loop_id: None,
            records: Vec::new(),
            metadata: Value::Null,
        }
    }
}

impl DeterministicFanoutResultRecord {
    pub fn new(
        fanout_id: impl Into<String>,
        record_id: impl Into<String>,
        status: DeterministicLoopStatus,
    ) -> Self {
        Self {
            schema: DETERMINISTIC_FANOUT_RESULT_SCHEMA.to_string(),
            fanout_id: fanout_id.into(),
            record_id: record_id.into(),
            status,
            evidence: Vec::new(),
            output: Value::Null,
            diagnostics: Value::Null,
        }
    }
}

impl DeterministicEvidenceSnapshot {
    pub fn new(
        snapshot_id: impl Into<String>,
        identity: DeterministicLoopRunIdentity,
        status: DeterministicLoopStatus,
    ) -> Self {
        Self {
            schema: DETERMINISTIC_EVIDENCE_SNAPSHOT_SCHEMA.to_string(),
            snapshot_id: snapshot_id.into(),
            identity,
            status,
            fanout_results: Vec::new(),
            evidence: Vec::new(),
            metadata: Value::Null,
        }
    }
}

impl Default for DeterministicLoopSpec {
    fn default() -> Self {
        Self {
            schema: DETERMINISTIC_LOOP_SPEC_SCHEMA.to_string(),
            loop_id: String::new(),
            max_iterations: default_max_iterations(),
            stop: DeterministicLoopStopCriteria::default(),
            retry: DeterministicLoopRetryPolicy::default(),
            artifacts: Vec::new(),
            validation: DeterministicLoopValidationPolicy::default(),
            metadata: Value::Null,
        }
    }
}

impl Default for DeterministicLoopStopCriteria {
    fn default() -> Self {
        Self {
            statuses: default_stop_statuses(),
        }
    }
}

impl Default for DeterministicLoopRetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: default_max_attempts(),
            statuses: default_retry_statuses(),
        }
    }
}

impl DeterministicLoopArtifactDeclaration {
    pub fn new(artifact_id: impl Into<String>) -> Self {
        Self {
            schema: DETERMINISTIC_LOOP_ARTIFACT_DECLARATION_SCHEMA.to_string(),
            artifact_id: artifact_id.into(),
            kind: None,
            required: false,
            local_evidence_required: false,
        }
    }
}

impl DeterministicLoopState {
    pub fn new(identity: DeterministicLoopRunIdentity) -> Self {
        Self {
            schema: DETERMINISTIC_LOOP_STATE_SCHEMA.to_string(),
            identity,
            status: DeterministicLoopStatus::Planned,
            iteration: 0,
            events: Vec::new(),
            artifact_lineage: Vec::new(),
            metadata: Value::Null,
        }
    }

    pub fn push_event(
        &mut self,
        event_type: impl Into<String>,
        status: DeterministicLoopStatus,
        artifact_refs: Vec<DeterministicEvidenceRef>,
        payload: Value,
    ) {
        self.events.push(DeterministicLoopEvent {
            schema: DETERMINISTIC_LOOP_EVENT_SCHEMA.to_string(),
            sequence: self.events.len() as u32 + 1,
            iteration: self.iteration,
            event_type: event_type.into(),
            status,
            artifact_refs,
            payload,
        });
    }
}

impl DeterministicLoopReconcileResult {
    pub fn new(status: DeterministicLoopStatus) -> Self {
        Self {
            status,
            artifact_refs: Vec::new(),
            metadata: Value::Null,
        }
    }
}

pub fn run_deterministic_loop<H>(
    spec: DeterministicLoopSpec,
    identity: DeterministicLoopRunIdentity,
    hooks: &mut H,
) -> Result<DeterministicLoopState>
where
    H: DeterministicLoopHooks,
{
    let mut sink = NoopDeterministicLoopEventSink;
    let cancellation = NeverCancelDeterministicLoop;
    run_deterministic_loop_with_runtime(
        spec,
        identity,
        hooks,
        DeterministicLoopRunOptions::default(),
        &mut sink,
        &cancellation,
    )
}

pub fn resume_deterministic_loop<H>(
    spec: DeterministicLoopSpec,
    identity: DeterministicLoopRunIdentity,
    resume_from: DeterministicLoopState,
    hooks: &mut H,
) -> Result<DeterministicLoopState>
where
    H: DeterministicLoopHooks,
{
    let mut sink = NoopDeterministicLoopEventSink;
    let cancellation = NeverCancelDeterministicLoop;
    run_deterministic_loop_with_runtime(
        spec,
        identity,
        hooks,
        DeterministicLoopRunOptions {
            resume_from: Some(resume_from),
        },
        &mut sink,
        &cancellation,
    )
}

pub fn run_deterministic_loop_with_runtime<H, S, C>(
    spec: DeterministicLoopSpec,
    identity: DeterministicLoopRunIdentity,
    hooks: &mut H,
    options: DeterministicLoopRunOptions,
    sink: &mut S,
    cancellation: &C,
) -> Result<DeterministicLoopState>
where
    H: DeterministicLoopHooks,
    S: DeterministicLoopEventSink,
    C: DeterministicLoopCancellation,
{
    validate_loop_spec(&spec)?;
    if identity.loop_id != spec.loop_id {
        return Err(Error::validation_invalid_argument(
            "identity.loop_id",
            "deterministic loop identity must match spec loop_id",
            Some(identity.loop_id),
            Some(vec![spec.loop_id]),
        ));
    }

    let mut state = options
        .resume_from
        .unwrap_or_else(|| DeterministicLoopState::new(identity.clone()));
    if state.identity != identity {
        return Err(Error::validation_invalid_argument(
            "resume_from.identity",
            "resume state identity must match requested loop identity",
            Some(state.identity.run_id.clone()),
            Some(vec![identity.run_id]),
        ));
    }
    if state.iteration > spec.max_iterations {
        return Err(Error::validation_invalid_argument(
            "resume_from.iteration",
            "resume state iteration cannot exceed spec max_iterations",
            Some(state.iteration.to_string()),
            Some(vec![spec.max_iterations.to_string()]),
        ));
    }
    if state.events.is_empty() {
        state.metadata = spec.metadata.clone();
        state.status = DeterministicLoopStatus::Running;
        push_event_and_checkpoint(
            &mut state,
            sink,
            "loop.started",
            DeterministicLoopStatus::Running,
            Vec::new(),
            serde_json::json!({ "max_iterations": spec.max_iterations }),
        )?;
    } else {
        state.status = DeterministicLoopStatus::Running;
        push_event_and_checkpoint(
            &mut state,
            sink,
            "loop.resumed",
            DeterministicLoopStatus::Running,
            Vec::new(),
            serde_json::json!({ "max_iterations": spec.max_iterations }),
        )?;
    }

    while state.iteration < spec.max_iterations {
        if cancellation.is_canceled(&state)? {
            state.status = DeterministicLoopStatus::Canceled;
            push_event_and_checkpoint(
                &mut state,
                sink,
                "loop.canceled",
                DeterministicLoopStatus::Canceled,
                Vec::new(),
                Value::Null,
            )?;
            break;
        }
        state.iteration += 1;
        let mut attempts = 0;
        let reconciliation = loop {
            attempts += 1;
            let plan = hooks.materialize_iteration(&state)?;
            push_event_and_checkpoint(
                &mut state,
                sink,
                "iteration.materialized",
                DeterministicLoopStatus::Running,
                Vec::new(),
                serde_json::json!({ "attempt": attempts }),
            )?;
            let output = hooks.execute_iteration(&state, plan)?;
            push_event_and_checkpoint(
                &mut state,
                sink,
                "iteration.executed",
                DeterministicLoopStatus::Running,
                Vec::new(),
                serde_json::json!({ "attempt": attempts }),
            )?;
            let reconciliation = hooks.reconcile_iteration(&state, output)?;
            if attempts >= spec.retry.max_attempts
                || !spec.retry.statuses.contains(&reconciliation.status)
            {
                break reconciliation;
            }
            push_event_and_checkpoint(
                &mut state,
                sink,
                "iteration.retry_scheduled",
                reconciliation.status,
                reconciliation.artifact_refs.clone(),
                serde_json::json!({
                    "attempt": attempts,
                    "max_attempts": spec.retry.max_attempts,
                }),
            )?;
        };
        state.status = reconciliation.status;
        state
            .artifact_lineage
            .extend(reconciliation.artifact_refs.clone());
        if !reconciliation.metadata.is_null() {
            state.metadata = merge_metadata(state.metadata, reconciliation.metadata);
        }
        push_event_and_checkpoint(
            &mut state,
            sink,
            "iteration.reconciled",
            state.status,
            reconciliation.artifact_refs,
            Value::Null,
        )?;
        if spec.stop.statuses.contains(&state.status) {
            break;
        }
    }

    if state.iteration >= spec.max_iterations && !spec.stop.statuses.contains(&state.status) {
        state.status = DeterministicLoopStatus::Blocked;
        push_event_and_checkpoint(
            &mut state,
            sink,
            "loop.max_iterations_reached",
            state.status,
            Vec::new(),
            serde_json::json!({ "max_iterations": spec.max_iterations }),
        )?;
    }
    validate_deterministic_loop_result(&state, &spec)?;
    push_event_and_checkpoint(
        &mut state,
        sink,
        "loop.finished",
        state.status,
        Vec::new(),
        Value::Null,
    )?;
    Ok(state)
}

pub fn validate_deterministic_loop_result(
    state: &DeterministicLoopState,
    spec: &DeterministicLoopSpec,
) -> Result<()> {
    for artifact in spec.artifacts.iter().filter(|artifact| artifact.required) {
        let matched = state.artifact_lineage.iter().any(|reference| {
            reference.label.as_deref() == Some(artifact.artifact_id.as_str())
                || reference.uri == artifact.artifact_id
                || artifact
                    .kind
                    .as_ref()
                    .is_some_and(|kind| reference.kind.as_deref() == Some(kind.as_str()))
        });
        if spec.validation.require_declared_artifacts && !matched {
            return Err(Error::validation_invalid_argument(
                "artifacts",
                format!(
                    "required artifact '{}' was not produced",
                    artifact.artifact_id
                ),
                Some(state.identity.run_id.clone()),
                None,
            ));
        }
    }

    if spec.validation.require_local_evidence
        || spec
            .artifacts
            .iter()
            .any(|artifact| artifact.local_evidence_required)
    {
        let has_local_evidence = state.artifact_lineage.iter().any(|reference| {
            reference.uri.starts_with("file://")
                || reference.uri.starts_with("./")
                || reference.uri.starts_with('/')
        });
        if !has_local_evidence {
            return Err(Error::validation_invalid_argument(
                "artifact_lineage",
                "loop result requires at least one local evidence reference",
                Some(state.identity.run_id.clone()),
                None,
            ));
        }
    }

    Ok(())
}

fn push_event_and_checkpoint<S>(
    state: &mut DeterministicLoopState,
    sink: &mut S,
    event_type: impl Into<String>,
    status: DeterministicLoopStatus,
    artifact_refs: Vec<DeterministicEvidenceRef>,
    payload: Value,
) -> Result<()>
where
    S: DeterministicLoopEventSink,
{
    state.push_event(event_type, status, artifact_refs, payload);
    let event = state
        .events
        .last()
        .expect("push_event always appends one event")
        .clone();
    sink.record_event(&event)?;
    sink.record_checkpoint(state)
}

fn validate_loop_spec(spec: &DeterministicLoopSpec) -> Result<()> {
    if spec.schema != DETERMINISTIC_LOOP_SPEC_SCHEMA {
        return Err(Error::validation_invalid_argument(
            "schema",
            format!(
                "expected {DETERMINISTIC_LOOP_SPEC_SCHEMA}, got {}",
                spec.schema
            ),
            Some(spec.loop_id.clone()),
            None,
        ));
    }
    if spec.loop_id.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "loop_id",
            "deterministic loop spec requires a non-empty loop_id",
            None,
            None,
        ));
    }
    if spec.max_iterations == 0 {
        return Err(Error::validation_invalid_argument(
            "max_iterations",
            "deterministic loop spec requires max_iterations greater than zero",
            Some(spec.loop_id.clone()),
            None,
        ));
    }
    if spec.retry.max_attempts == 0 {
        return Err(Error::validation_invalid_argument(
            "retry.max_attempts",
            "deterministic loop retry policy requires max_attempts greater than zero",
            Some(spec.loop_id.clone()),
            None,
        ));
    }
    for artifact in &spec.artifacts {
        if artifact.schema != DETERMINISTIC_LOOP_ARTIFACT_DECLARATION_SCHEMA {
            return Err(Error::validation_invalid_argument(
                "artifacts.schema",
                format!(
                    "expected {DETERMINISTIC_LOOP_ARTIFACT_DECLARATION_SCHEMA}, got {}",
                    artifact.schema
                ),
                Some(spec.loop_id.clone()),
                None,
            ));
        }
        if artifact.artifact_id.trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                "artifacts.artifact_id",
                "deterministic loop artifact declarations require a non-empty artifact_id",
                Some(spec.loop_id.clone()),
                None,
            ));
        }
    }
    Ok(())
}

fn merge_metadata(current: Value, next: Value) -> Value {
    match (current, next) {
        (Value::Object(mut current), Value::Object(next)) => {
            current.extend(next);
            Value::Object(current)
        }
        (_, next) => next,
    }
}

fn loop_plan_schema() -> String {
    DETERMINISTIC_LOOP_PLAN_SCHEMA.to_string()
}

fn run_identity_schema() -> String {
    DETERMINISTIC_LOOP_RUN_IDENTITY_SCHEMA.to_string()
}

fn fanout_plan_schema() -> String {
    DETERMINISTIC_FANOUT_PLAN_SCHEMA.to_string()
}

fn fanout_result_schema() -> String {
    DETERMINISTIC_FANOUT_RESULT_SCHEMA.to_string()
}

fn evidence_snapshot_schema() -> String {
    DETERMINISTIC_EVIDENCE_SNAPSHOT_SCHEMA.to_string()
}

fn loop_spec_schema() -> String {
    DETERMINISTIC_LOOP_SPEC_SCHEMA.to_string()
}

fn loop_state_schema() -> String {
    DETERMINISTIC_LOOP_STATE_SCHEMA.to_string()
}

fn loop_event_schema() -> String {
    DETERMINISTIC_LOOP_EVENT_SCHEMA.to_string()
}

fn artifact_declaration_schema() -> String {
    DETERMINISTIC_LOOP_ARTIFACT_DECLARATION_SCHEMA.to_string()
}

fn default_max_iterations() -> u32 {
    1
}

fn default_max_attempts() -> u32 {
    1
}

fn default_stop_statuses() -> Vec<DeterministicLoopStatus> {
    vec![
        DeterministicLoopStatus::Succeeded,
        DeterministicLoopStatus::Failed,
        DeterministicLoopStatus::Canceled,
        DeterministicLoopStatus::Blocked,
    ]
}

fn default_retry_statuses() -> Vec<DeterministicLoopStatus> {
    vec![DeterministicLoopStatus::Failed]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_loop_contract_serializes_stable_schema_and_status_values() {
        let mut identity = DeterministicLoopRunIdentity::new("loop-a", "run-a");
        identity.task_id = Some("task-a".to_string());
        identity.group_key = Some("group-a".to_string());
        identity.attempt_id = Some("attempt-a".to_string());
        let mut fanout = DeterministicFanoutPlan::new("fanout-a");
        fanout.loop_id = Some(identity.loop_id.clone());
        fanout.records.push(DeterministicFanoutPlanRecord {
            record_id: "record-a".to_string(),
            dedupe_key: "loop-a:record-a".to_string(),
            depends_on: Vec::new(),
            input: Value::Null,
        });

        let mut plan = DeterministicLoopPlan::new(identity.clone());
        plan.fanouts.push(fanout);

        let value = serde_json::to_value(&plan).expect("loop plan serializes");
        assert_eq!(value["schema"], DETERMINISTIC_LOOP_PLAN_SCHEMA);
        assert_eq!(
            value["identity"]["schema"],
            DETERMINISTIC_LOOP_RUN_IDENTITY_SCHEMA
        );
        assert_eq!(value["identity"]["task_id"], "task-a");
        assert_eq!(value["identity"]["group_key"], "group-a");
        assert_eq!(value["identity"]["attempt_id"], "attempt-a");
        assert_eq!(value["status"], "planned");
        assert_eq!(
            value["fanouts"][0]["schema"],
            DETERMINISTIC_FANOUT_PLAN_SCHEMA
        );

        let snapshot = DeterministicEvidenceSnapshot::new(
            "snapshot-a",
            identity,
            DeterministicLoopStatus::Succeeded,
        );
        let snapshot_value = serde_json::to_value(&snapshot).expect("snapshot serializes");
        assert_eq!(
            snapshot_value["schema"],
            DETERMINISTIC_EVIDENCE_SNAPSHOT_SCHEMA
        );
        assert_eq!(snapshot_value["status"], "succeeded");
    }

    #[derive(Default)]
    struct CountingHooks {
        statuses: Vec<DeterministicLoopStatus>,
        artifact_uri: Option<String>,
        materialized: u32,
    }

    impl DeterministicLoopHooks for CountingHooks {
        type IterationPlan = u32;
        type IterationOutput = DeterministicLoopStatus;

        fn materialize_iteration(
            &mut self,
            state: &DeterministicLoopState,
        ) -> Result<Self::IterationPlan> {
            self.materialized += 1;
            Ok(state.iteration)
        }

        fn execute_iteration(
            &mut self,
            _state: &DeterministicLoopState,
            _plan: Self::IterationPlan,
        ) -> Result<Self::IterationOutput> {
            Ok(self
                .statuses
                .get(self.materialized as usize - 1)
                .copied()
                .unwrap_or(DeterministicLoopStatus::Running))
        }

        fn reconcile_iteration(
            &mut self,
            _state: &DeterministicLoopState,
            output: Self::IterationOutput,
        ) -> Result<DeterministicLoopReconcileResult> {
            let mut result = DeterministicLoopReconcileResult::new(output);
            result.artifact_refs.push(DeterministicEvidenceRef {
                uri: self
                    .artifact_uri
                    .clone()
                    .unwrap_or_else(|| "artifact://iteration".to_string()),
                kind: Some("test".to_string()),
                label: None,
            });
            Ok(result)
        }
    }

    #[test]
    fn deterministic_loop_runs_hooks_until_stop_status() {
        let spec = DeterministicLoopSpec {
            loop_id: "loop-a".to_string(),
            max_iterations: 3,
            ..DeterministicLoopSpec::default()
        };
        let identity = DeterministicLoopRunIdentity::new("loop-a", "run-a");
        let mut hooks = CountingHooks {
            statuses: vec![
                DeterministicLoopStatus::Running,
                DeterministicLoopStatus::Succeeded,
            ],
            ..CountingHooks::default()
        };

        let state = run_deterministic_loop(spec, identity, &mut hooks).expect("loop runs");

        assert_eq!(state.status, DeterministicLoopStatus::Succeeded);
        assert_eq!(state.iteration, 2);
        assert_eq!(state.artifact_lineage.len(), 2);
        assert_eq!(state.events.last().unwrap().event_type, "loop.finished");
    }

    #[test]
    fn deterministic_loop_blocks_when_max_iterations_are_exhausted() {
        let spec = DeterministicLoopSpec {
            loop_id: "loop-a".to_string(),
            max_iterations: 2,
            ..DeterministicLoopSpec::default()
        };
        let identity = DeterministicLoopRunIdentity::new("loop-a", "run-a");
        let mut hooks = CountingHooks {
            statuses: vec![
                DeterministicLoopStatus::Running,
                DeterministicLoopStatus::Running,
            ],
            ..CountingHooks::default()
        };

        let state = run_deterministic_loop(spec, identity, &mut hooks).expect("loop runs");

        assert_eq!(state.status, DeterministicLoopStatus::Blocked);
        assert_eq!(state.iteration, 2);
        assert!(state
            .events
            .iter()
            .any(|event| event.event_type == "loop.max_iterations_reached"));
    }

    #[derive(Default)]
    struct RecordingSink {
        checkpoints: usize,
        events: Vec<String>,
    }

    impl DeterministicLoopEventSink for RecordingSink {
        fn record_checkpoint(&mut self, _state: &DeterministicLoopState) -> Result<()> {
            self.checkpoints += 1;
            Ok(())
        }

        fn record_event(&mut self, event: &DeterministicLoopEvent) -> Result<()> {
            self.events.push(event.event_type.clone());
            Ok(())
        }
    }

    struct CancelAfterStart;

    impl DeterministicLoopCancellation for CancelAfterStart {
        fn is_canceled(&self, state: &DeterministicLoopState) -> Result<bool> {
            Ok(state.iteration == 0)
        }
    }

    #[test]
    fn deterministic_loop_records_checkpoints_and_retries_retryable_statuses() {
        let spec = DeterministicLoopSpec {
            loop_id: "loop-a".to_string(),
            max_iterations: 1,
            retry: DeterministicLoopRetryPolicy {
                max_attempts: 2,
                statuses: vec![DeterministicLoopStatus::Failed],
            },
            ..DeterministicLoopSpec::default()
        };
        let identity = DeterministicLoopRunIdentity::new("loop-a", "run-a");
        let mut hooks = CountingHooks {
            statuses: vec![
                DeterministicLoopStatus::Failed,
                DeterministicLoopStatus::Succeeded,
            ],
            ..CountingHooks::default()
        };
        let mut sink = RecordingSink::default();
        let cancellation = NeverCancelDeterministicLoop;

        let state = run_deterministic_loop_with_runtime(
            spec,
            identity,
            &mut hooks,
            DeterministicLoopRunOptions::default(),
            &mut sink,
            &cancellation,
        )
        .expect("loop runs");

        assert_eq!(state.status, DeterministicLoopStatus::Succeeded);
        assert_eq!(hooks.materialized, 2);
        assert!(sink.checkpoints >= 1);
        assert!(sink
            .events
            .iter()
            .any(|event| event == "iteration.retry_scheduled"));
    }

    #[test]
    fn deterministic_loop_can_resume_from_checkpoint_and_cancel() {
        let spec = DeterministicLoopSpec {
            loop_id: "loop-a".to_string(),
            max_iterations: 2,
            ..DeterministicLoopSpec::default()
        };
        let identity = DeterministicLoopRunIdentity::new("loop-a", "run-a");
        let mut resume_from = DeterministicLoopState::new(identity.clone());
        resume_from.status = DeterministicLoopStatus::Waiting;
        resume_from.iteration = 0;
        resume_from.push_event(
            "operator.paused",
            DeterministicLoopStatus::Waiting,
            Vec::new(),
            Value::Null,
        );
        let mut hooks = CountingHooks::default();
        let mut sink = RecordingSink::default();

        let state = run_deterministic_loop_with_runtime(
            spec,
            identity,
            &mut hooks,
            DeterministicLoopRunOptions {
                resume_from: Some(resume_from),
            },
            &mut sink,
            &CancelAfterStart,
        )
        .expect("loop resumes");

        assert_eq!(state.status, DeterministicLoopStatus::Canceled);
        assert_eq!(state.iteration, 0);
        assert!(sink.events.iter().any(|event| event == "loop.resumed"));
        assert!(sink.events.iter().any(|event| event == "loop.canceled"));
    }

    #[test]
    fn deterministic_loop_validates_declared_and_local_evidence() {
        let mut artifact = DeterministicLoopArtifactDeclaration::new("artifact-a");
        artifact.kind = Some("test".to_string());
        artifact.required = true;
        artifact.local_evidence_required = true;
        let spec = DeterministicLoopSpec {
            loop_id: "loop-a".to_string(),
            max_iterations: 1,
            artifacts: vec![artifact],
            validation: DeterministicLoopValidationPolicy {
                require_declared_artifacts: true,
                require_local_evidence: true,
            },
            ..DeterministicLoopSpec::default()
        };
        let identity = DeterministicLoopRunIdentity::new("loop-a", "run-a");
        let mut hooks = CountingHooks {
            statuses: vec![DeterministicLoopStatus::Succeeded],
            artifact_uri: Some("file:///tmp/artifact-a.json".to_string()),
            ..CountingHooks::default()
        };

        let state = run_deterministic_loop(spec, identity, &mut hooks).expect("loop validates");

        assert_eq!(state.status, DeterministicLoopStatus::Succeeded);
        assert_eq!(state.artifact_lineage[0].uri, "file:///tmp/artifact-a.json");
    }
}
