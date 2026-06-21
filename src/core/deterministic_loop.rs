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

impl DeterministicLoopRunIdentity {
    pub fn new(loop_id: impl Into<String>, run_id: impl Into<String>) -> Self {
        Self {
            schema: DETERMINISTIC_LOOP_RUN_IDENTITY_SCHEMA.to_string(),
            loop_id: loop_id.into(),
            run_id: run_id.into(),
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
    validate_loop_spec(&spec)?;
    if identity.loop_id != spec.loop_id {
        return Err(Error::validation_invalid_argument(
            "identity.loop_id",
            "deterministic loop identity must match spec loop_id",
            Some(identity.loop_id),
            Some(vec![spec.loop_id]),
        ));
    }

    let mut state = DeterministicLoopState::new(identity);
    state.metadata = spec.metadata.clone();
    state.status = DeterministicLoopStatus::Running;
    state.push_event(
        "loop.started",
        state.status,
        Vec::new(),
        serde_json::json!({ "max_iterations": spec.max_iterations }),
    );

    while state.iteration < spec.max_iterations {
        state.iteration += 1;
        let plan = hooks.materialize_iteration(&state)?;
        state.push_event(
            "iteration.materialized",
            DeterministicLoopStatus::Running,
            Vec::new(),
            Value::Null,
        );
        let output = hooks.execute_iteration(&state, plan)?;
        state.push_event(
            "iteration.executed",
            DeterministicLoopStatus::Running,
            Vec::new(),
            Value::Null,
        );
        let reconciliation = hooks.reconcile_iteration(&state, output)?;
        state.status = reconciliation.status;
        state
            .artifact_lineage
            .extend(reconciliation.artifact_refs.clone());
        if !reconciliation.metadata.is_null() {
            state.metadata = merge_metadata(state.metadata, reconciliation.metadata);
        }
        state.push_event(
            "iteration.reconciled",
            state.status,
            reconciliation.artifact_refs,
            Value::Null,
        );
        if spec.stop.statuses.contains(&state.status) {
            break;
        }
    }

    if state.iteration >= spec.max_iterations && !spec.stop.statuses.contains(&state.status) {
        state.status = DeterministicLoopStatus::Blocked;
        state.push_event(
            "loop.max_iterations_reached",
            state.status,
            Vec::new(),
            serde_json::json!({ "max_iterations": spec.max_iterations }),
        );
    }
    state.push_event("loop.finished", state.status, Vec::new(), Value::Null);
    Ok(state)
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

fn default_max_iterations() -> u32 {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_loop_contract_serializes_stable_schema_and_status_values() {
        let identity = DeterministicLoopRunIdentity::new("loop-a", "run-a");
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
    }

    impl DeterministicLoopHooks for CountingHooks {
        type IterationPlan = u32;
        type IterationOutput = DeterministicLoopStatus;

        fn materialize_iteration(
            &mut self,
            state: &DeterministicLoopState,
        ) -> Result<Self::IterationPlan> {
            Ok(state.iteration)
        }

        fn execute_iteration(
            &mut self,
            _state: &DeterministicLoopState,
            plan: Self::IterationPlan,
        ) -> Result<Self::IterationOutput> {
            Ok(self
                .statuses
                .get(plan as usize - 1)
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
                uri: "artifact://iteration".to_string(),
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
        };

        let state = run_deterministic_loop(spec, identity, &mut hooks).expect("loop runs");

        assert_eq!(state.status, DeterministicLoopStatus::Blocked);
        assert_eq!(state.iteration, 2);
        assert!(state
            .events
            .iter()
            .any(|event| event.event_type == "loop.max_iterations_reached"));
    }
}
