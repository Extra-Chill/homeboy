//! Versioned control-plane resources and the shared result envelope.
//!
//! Resources and envelopes are versioned once. Callers do not mint a schema
//! per method.

use serde::{Deserialize, Serialize};

use crate::identity::{
    AttemptId, ExecutionId, MissionCursor, MissionId, ProviderSessionId, RunCursor, RunId,
    TaskCursor, TaskId,
};

pub const CONTROL_PLANE_RESULT_SCHEMA: &str = "homeboy/control-plane-result/v1";
pub const CONTROL_PLANE_RUN_SCHEMA: &str = "homeboy/control-plane-run/v1";
pub const CONTROL_PLANE_RUN_PAGE_SCHEMA: &str = "homeboy/control-plane-run-page/v1";
pub const CONTROL_PLANE_MISSION_SCHEMA: &str = "homeboy/control-plane-mission/v1";
pub const CONTROL_PLANE_MISSION_PAGE_SCHEMA: &str = "homeboy/control-plane-mission-page/v1";
pub const CONTROL_PLANE_TASK_SCHEMA: &str = "homeboy/control-plane-task/v1";
pub const CONTROL_PLANE_TASK_PAGE_SCHEMA: &str = "homeboy/control-plane-task-page/v1";
pub const CONTROL_PLANE_ACTION_ELIGIBILITY_SCHEMA: &str =
    "homeboy/control-plane-action-eligibility/v1";

/// Shared result envelope for every control-plane operation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneResult<T> {
    pub schema: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ControlPlaneError>,
}

impl<T> ControlPlaneResult<T> {
    pub fn ok(resource: T) -> Self {
        Self {
            schema: CONTROL_PLANE_RESULT_SCHEMA.to_string(),
            ok: true,
            resource: Some(resource),
            error: None,
        }
    }

    pub fn err(error: ControlPlaneError) -> Self {
        Self {
            schema: CONTROL_PLANE_RESULT_SCHEMA.to_string(),
            ok: false,
            resource: None,
            error: Some(error),
        }
    }
}

/// Typed failure carried by the result envelope. HTTP adapters must not
/// flatten this to prose.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneError {
    pub class: ControlPlaneErrorClass,
    pub retryable: bool,
    pub message: String,
}

impl ControlPlaneError {
    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            class: ControlPlaneErrorClass::NotFound,
            retryable: false,
            message: message.into(),
        }
    }

    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self {
            class: ControlPlaneErrorClass::InvalidArgument,
            retryable: false,
            message: message.into(),
        }
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self {
            class: ControlPlaneErrorClass::Unavailable,
            retryable: true,
            message: message.into(),
        }
    }

    pub fn http_status(&self) -> u16 {
        match self.class {
            ControlPlaneErrorClass::NotFound => 404,
            ControlPlaneErrorClass::InvalidArgument => 400,
            ControlPlaneErrorClass::Unavailable => 503,
        }
    }
}

impl std::fmt::Display for ControlPlaneError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ControlPlaneError {}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ControlPlaneErrorClass {
    NotFound,
    InvalidArgument,
    Unavailable,
}

/// Canonical run resource. Pure, redacted, and non-reconciling.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneRun {
    pub schema: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mission: Option<MissionId>,
    pub run: RunId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<AttemptId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt_number: Option<u32>,
    pub state: ControlPlaneRunState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<ControlPlaneLocation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocker: Option<ControlPlaneBlocker>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<ControlPlaneOwner>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<ControlPlaneRuntime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<ControlPlaneProviderSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heartbeat_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub liveness: Option<ControlPlaneLiveness>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate: Option<ControlPlaneStateSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gates: Vec<ControlPlaneStateSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publication: Option<ControlPlaneStateSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_eligibility: Option<ControlPlaneActionEligibilityReport>,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<ControlPlaneEvidenceRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ControlPlaneEvidenceRef>,
}

impl ControlPlaneRun {
    pub fn new(run: RunId) -> Self {
        Self {
            schema: CONTROL_PLANE_RUN_SCHEMA.to_string(),
            mission: None,
            run: run.clone(),
            attempt: None,
            attempt_number: None,
            state: ControlPlaneRunState::Unknown,
            location: None,
            execution: None,
            phase: None,
            blocker: None,
            owner: None,
            runtime: None,
            provider: None,
            heartbeat_at: None,
            liveness: None,
            candidate: None,
            gates: Vec::new(),
            publication: None,
            action_eligibility: None,
            created_at: String::new(),
            updated_at: None,
            finished_at: None,
            evidence: Vec::new(),
            artifacts: Vec::new(),
        }
    }
}

/// Generic, bounded discovery parameters. Product-specific selectors remain
/// adapter-owned until they are shared by more than one control-plane client.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneRunListRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mission: Option<MissionId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<RunCursor>,
    pub limit: u32,
}

impl Default for ControlPlaneRunListRequest {
    fn default() -> Self {
        Self {
            mission: None,
            cursor: None,
            limit: 50,
        }
    }
}

impl ControlPlaneRunListRequest {
    pub fn validate(&self) -> Result<(), ControlPlaneError> {
        if !(1..=100).contains(&self.limit) {
            return Err(ControlPlaneError::invalid_argument(
                "control-plane run page limit must be between 1 and 100",
            ));
        }
        Ok(())
    }
}

/// One stable page of canonical run resources.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneRunPage {
    pub schema: String,
    pub runs: Vec<ControlPlaneRun>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<RunCursor>,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneMission {
    pub schema: String,
    pub mission: MissionId,
    pub created_at: String,
    pub updated_at: String,
    pub run_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneMissionListRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<MissionCursor>,
    pub limit: u32,
}

impl Default for ControlPlaneMissionListRequest {
    fn default() -> Self {
        Self {
            cursor: None,
            limit: 50,
        }
    }
}

impl ControlPlaneMissionListRequest {
    pub fn validate(&self) -> Result<(), ControlPlaneError> {
        if !(1..=100).contains(&self.limit) {
            return Err(ControlPlaneError::invalid_argument(
                "control-plane mission page limit must be between 1 and 100",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneMissionPage {
    pub schema: String,
    pub missions: Vec<ControlPlaneMission>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<MissionCursor>,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneTask {
    pub schema: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mission: Option<MissionId>,
    pub run: RunId,
    pub task: TaskId,
    pub state: ControlPlaneState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneTaskListRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<TaskCursor>,
    pub limit: u32,
}

impl Default for ControlPlaneTaskListRequest {
    fn default() -> Self {
        Self {
            cursor: None,
            limit: 50,
        }
    }
}

impl ControlPlaneTaskListRequest {
    pub fn validate(&self) -> Result<(), ControlPlaneError> {
        if !(1..=100).contains(&self.limit) {
            return Err(ControlPlaneError::invalid_argument(
                "control-plane task page limit must be between 1 and 100",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneTaskPage {
    pub schema: String,
    pub run: RunId,
    pub tasks: Vec<ControlPlaneTask>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<TaskCursor>,
    pub has_more: bool,
}

/// Bounded live provider evidence. This intentionally carries timestamps and a
/// source name only; provider output and filesystem paths remain out of status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneLiveness {
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_observed_progress_at: Option<String>,
    pub age_seconds: u64,
    pub window_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneBlocker {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneOwner {
    pub kind: String,
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneRuntime {
    pub build_identity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneProviderSummary {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<ProviderSessionId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneStateSummary {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub state: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ControlPlaneAction {
    Cancel,
    Resume,
    Retry,
    Promote,
    Reconcile,
}

impl ControlPlaneAction {
    pub const fn is_mutating(self) -> bool {
        true
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ControlPlaneActionConfirmation {
    None,
    Required,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ControlPlaneActionAvailability {
    Available,
    Unavailable,
    Indeterminate,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneActionEligibility {
    pub action: ControlPlaneAction,
    pub availability: ControlPlaneActionAvailability,
    pub reason: String,
    pub confirmation: ControlPlaneActionConfirmation,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_inputs: Vec<String>,
    pub idempotent: bool,
    pub requires_revalidation: bool,
    pub result_resource_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneActionEligibilityReport {
    pub schema: String,
    pub run: RunId,
    pub actions: Vec<ControlPlaneActionEligibility>,
}

impl ControlPlaneActionEligibilityReport {
    pub fn new(run: RunId) -> Self {
        Self {
            schema: CONTROL_PLANE_ACTION_ELIGIBILITY_SCHEMA.to_string(),
            run,
            actions: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ControlPlaneState {
    Queued,
    Blocked,
    Skipped,
    Running,
    Succeeded,
    CandidateRecoverable,
    PartialRecoverable,
    PartialFailure,
    Failed,
    Cancelled,
    TimedOut,
    Stale,
    #[serde(other)]
    Unknown,
}

impl ControlPlaneState {
    pub fn is_terminal(self) -> bool {
        !matches!(
            self,
            Self::Queued | Self::Blocked | Self::Running | Self::Unknown
        )
    }
}

pub type ControlPlaneRunState = ControlPlaneState;

/// Where the run is executing. Ids and transport only — never cwd or secrets.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneLocation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_run_id: Option<String>,
}

/// Evidence or artifact pointer. The URI is a reference, not payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneEvidenceRef {
    pub id: String,
    pub kind: String,
    pub uri: String,
}

#[cfg(test)]
mod tests {
    use super::{
        ControlPlaneAction, ControlPlaneActionAvailability, ControlPlaneActionConfirmation,
        ControlPlaneActionEligibility, ControlPlaneActionEligibilityReport, ControlPlaneBlocker,
        ControlPlaneError, ControlPlaneErrorClass, ControlPlaneEvidenceRef, ControlPlaneLiveness,
        ControlPlaneLocation, ControlPlaneMission, ControlPlaneMissionListRequest,
        ControlPlaneMissionPage, ControlPlaneOwner, ControlPlaneProviderSummary,
        ControlPlaneResult, ControlPlaneRun, ControlPlaneRunListRequest, ControlPlaneRunPage,
        ControlPlaneRunState, ControlPlaneRuntime, ControlPlaneState, ControlPlaneStateSummary,
        ControlPlaneTask, ControlPlaneTaskListRequest, ControlPlaneTaskPage,
        CONTROL_PLANE_ACTION_ELIGIBILITY_SCHEMA, CONTROL_PLANE_MISSION_PAGE_SCHEMA,
        CONTROL_PLANE_MISSION_SCHEMA, CONTROL_PLANE_RESULT_SCHEMA, CONTROL_PLANE_RUN_PAGE_SCHEMA,
        CONTROL_PLANE_RUN_SCHEMA, CONTROL_PLANE_TASK_PAGE_SCHEMA, CONTROL_PLANE_TASK_SCHEMA,
    };
    use crate::{
        AttemptId, ExecutionId, MissionCursor, MissionId, ProviderSessionId, RunCursor, RunId,
        TaskCursor, TaskId,
    };

    const AGENT_TASK_COOK: &str = "agent-task-301a2b9a-a63d-446b-a918-e21b2ff6421e";
    const AGENT_TASK_RUN: &str =
        "agent-task-301a2b9a-a63d-446b-a918-e21b2ff6421e-attempt-1-ea6a6751";

    fn sample_run() -> ControlPlaneRun {
        let run = RunId::new(AGENT_TASK_RUN).expect("run");
        let mut resource = ControlPlaneRun::new(run.clone());
        resource.mission = Some(MissionId::new(AGENT_TASK_COOK).expect("mission"));
        resource.attempt = Some(AttemptId::new(AGENT_TASK_RUN).expect("attempt"));
        resource.attempt_number = Some(1);
        resource.state = ControlPlaneRunState::Succeeded;
        resource.location = Some(ControlPlaneLocation {
            runner_id: Some("homeboy-lab".to_string()),
            remote_run_id: Some("remote-run-1".to_string()),
        });
        resource.execution = Some(ExecutionId::new("job-1").expect("execution"));
        resource.phase = Some("terminal".to_string());
        resource.blocker = Some(ControlPlaneBlocker {
            code: Some("stale".to_string()),
            message: "runner_disconnected".to_string(),
        });
        resource.owner = Some(ControlPlaneOwner {
            kind: "runner".to_string(),
            id: "homeboy-lab".to_string(),
        });
        resource.runtime = Some(ControlPlaneRuntime {
            build_identity: "homeboy 0.1.0+test".to_string(),
        });
        resource.provider = Some(ControlPlaneProviderSummary {
            id: "claude".to_string(),
            state: Some("succeeded".to_string()),
            session: Some(ProviderSessionId::new("sess-1").expect("session")),
        });
        resource.heartbeat_at = Some("2026-01-01T00:00:30Z".to_string());
        resource.liveness = Some(ControlPlaneLiveness {
            state: "active".to_string(),
            source: Some("structured_progress".to_string()),
            last_observed_progress_at: Some("2026-01-01T00:00:45Z".to_string()),
            age_seconds: 15,
            window_seconds: 300,
        });
        resource.candidate = Some(ControlPlaneStateSummary {
            id: Some("review".to_string()),
            state: "applied".to_string(),
        });
        resource.gates = vec![ControlPlaneStateSummary {
            id: Some("test".to_string()),
            state: "passed".to_string(),
        }];
        resource.publication = Some(ControlPlaneStateSummary {
            id: Some("https://example.invalid/pr/1".to_string()),
            state: "published".to_string(),
        });
        resource.action_eligibility = Some(ControlPlaneActionEligibilityReport {
            schema: CONTROL_PLANE_ACTION_ELIGIBILITY_SCHEMA.to_string(),
            run: run.clone(),
            actions: vec![ControlPlaneActionEligibility {
                action: ControlPlaneAction::Cancel,
                availability: ControlPlaneActionAvailability::Available,
                reason: "cancellation is available".to_string(),
                confirmation: ControlPlaneActionConfirmation::None,
                required_inputs: Vec::new(),
                idempotent: true,
                requires_revalidation: true,
                result_resource_type: "run".to_string(),
            }],
        });
        resource.created_at = "2026-01-01T00:00:00Z".to_string();
        resource.updated_at = Some("2026-01-01T00:01:00Z".to_string());
        resource.finished_at = Some("2026-01-01T00:01:00Z".to_string());
        resource.evidence = vec![ControlPlaneEvidenceRef {
            id: "outcome".to_string(),
            kind: "outcome".to_string(),
            uri: "homeboy://evidence/outcome".to_string(),
        }];
        resource.artifacts = vec![ControlPlaneEvidenceRef {
            id: "review".to_string(),
            kind: "review_form".to_string(),
            uri: "homeboy://artifact/review".to_string(),
        }];
        resource
    }

    #[test]
    fn control_plane_run_round_trips_through_serde_with_typed_identities() {
        let resource = sample_run();
        let value = serde_json::to_value(&resource).expect("serialize");
        assert_eq!(value["schema"], CONTROL_PLANE_RUN_SCHEMA);
        assert_eq!(value["mission"], AGENT_TASK_COOK);
        assert_eq!(value["run"], AGENT_TASK_RUN);
        assert_eq!(value["attempt"], AGENT_TASK_RUN);
        assert_eq!(value["attempt_number"], 1);
        assert_eq!(value["state"], "succeeded");
        assert_eq!(value["phase"], "terminal");
        assert_eq!(value["owner"]["kind"], "runner");
        assert_eq!(value["runtime"]["build_identity"], "homeboy 0.1.0+test");
        assert_eq!(value["provider"]["id"], "claude");
        assert_eq!(value["heartbeat_at"], "2026-01-01T00:00:30Z");
        assert_eq!(value["liveness"]["state"], "active");
        assert_eq!(value["candidate"]["state"], "applied");
        assert_eq!(value["gates"][0]["id"], "test");
        assert_eq!(value["publication"]["state"], "published");
        assert_eq!(
            value["action_eligibility"]["schema"],
            CONTROL_PLANE_ACTION_ELIGIBILITY_SCHEMA
        );
        assert!(value.get("metadata").is_none());
        assert!(value.get("cwd").is_none());
        assert!(value.get("prompt").is_none());
        let decoded: ControlPlaneRun = serde_json::from_value(value).expect("deserialize");
        assert_eq!(decoded, resource);
    }

    #[test]
    fn run_page_round_trips_with_a_bounded_typed_cursor() {
        let page = ControlPlaneRunPage {
            schema: CONTROL_PLANE_RUN_PAGE_SCHEMA.to_string(),
            runs: vec![sample_run()],
            next_cursor: Some(RunCursor::new(AGENT_TASK_RUN).expect("cursor")),
            has_more: true,
        };
        let value = serde_json::to_value(&page).expect("serialize");
        assert_eq!(value["schema"], CONTROL_PLANE_RUN_PAGE_SCHEMA);
        assert_eq!(value["next_cursor"], AGENT_TASK_RUN);
        let decoded: ControlPlaneRunPage = serde_json::from_value(value).expect("deserialize");
        assert_eq!(decoded, page);

        let request = ControlPlaneRunListRequest {
            mission: Some(MissionId::new(AGENT_TASK_COOK).expect("mission")),
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_value(request).expect("serialize request")["mission"],
            AGENT_TASK_COOK
        );

        assert!(ControlPlaneRunListRequest {
            limit: 0,
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(ControlPlaneRunListRequest {
            limit: 101,
            ..Default::default()
        }
        .validate()
        .is_err());
    }

    #[test]
    fn run_page_v1_golden_fixture_remains_compatible() {
        let fixture = include_str!("../tests/fixtures/run-page-v1.json");
        let page: ControlPlaneRunPage = serde_json::from_str(fixture).expect("v1 fixture");
        assert_eq!(page.schema, CONTROL_PLANE_RUN_PAGE_SCHEMA);
        assert_eq!(page.runs[0].run.as_str(), "run-fixture-1");
        assert_eq!(page.runs[0].state, ControlPlaneRunState::Running);
        assert!(page.next_cursor.is_none());
        assert!(!page.has_more);
    }

    #[test]
    fn mission_resource_and_page_round_trip() {
        let mission = ControlPlaneMission {
            schema: CONTROL_PLANE_MISSION_SCHEMA.to_string(),
            mission: MissionId::new("mission-1").expect("mission"),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-02T00:00:00Z".to_string(),
            run_count: 2,
        };
        let page = ControlPlaneMissionPage {
            schema: CONTROL_PLANE_MISSION_PAGE_SCHEMA.to_string(),
            missions: vec![mission],
            next_cursor: Some(MissionCursor::new("opaque").expect("cursor")),
            has_more: true,
        };
        let value = serde_json::to_value(&page).expect("serialize");
        assert_eq!(
            serde_json::from_value::<ControlPlaneMissionPage>(value).expect("deserialize"),
            page
        );
        assert!(ControlPlaneMissionListRequest {
            limit: 101,
            ..Default::default()
        }
        .validate()
        .is_err());
    }

    #[test]
    fn run_scoped_task_resource_and_page_round_trip() {
        let task = ControlPlaneTask {
            schema: CONTROL_PLANE_TASK_SCHEMA.to_string(),
            mission: Some(MissionId::new(AGENT_TASK_COOK).expect("mission")),
            run: RunId::new(AGENT_TASK_RUN).expect("run"),
            task: TaskId::new("review").expect("task"),
            state: ControlPlaneState::Blocked,
        };
        let page = ControlPlaneTaskPage {
            schema: CONTROL_PLANE_TASK_PAGE_SCHEMA.to_string(),
            run: task.run.clone(),
            tasks: vec![task],
            next_cursor: Some(TaskCursor::new("opaque").expect("cursor")),
            has_more: true,
        };
        let value = serde_json::to_value(&page).expect("serialize");
        assert_eq!(
            serde_json::from_value::<ControlPlaneTaskPage>(value).expect("deserialize"),
            page
        );
        assert!(!ControlPlaneState::Blocked.is_terminal());
        assert!(ControlPlaneState::Skipped.is_terminal());
        assert!(ControlPlaneTaskListRequest {
            limit: 0,
            ..Default::default()
        }
        .validate()
        .is_err());
    }

    #[test]
    fn action_eligibility_report_round_trips_without_agent_task_schema() {
        let run = RunId::new(AGENT_TASK_RUN).expect("run");
        let report = ControlPlaneActionEligibilityReport {
            schema: CONTROL_PLANE_ACTION_ELIGIBILITY_SCHEMA.to_string(),
            run: run.clone(),
            actions: vec![ControlPlaneActionEligibility {
                action: ControlPlaneAction::Cancel,
                availability: ControlPlaneActionAvailability::Unavailable,
                reason: "run is terminal".to_string(),
                confirmation: ControlPlaneActionConfirmation::Required,
                required_inputs: Vec::new(),
                idempotent: false,
                requires_revalidation: true,
                result_resource_type: "run".to_string(),
            }],
        };
        let value = serde_json::to_value(&report).expect("serialize");
        assert_eq!(value["schema"], CONTROL_PLANE_ACTION_ELIGIBILITY_SCHEMA);
        assert_eq!(value["run"], AGENT_TASK_RUN);
        assert!(value.get("run_id").is_none());
        assert!(value.get("state").is_none());
        assert!(value.get("location").is_none());
        assert_eq!(value["actions"][0]["action"], "cancel");
        let decoded: ControlPlaneActionEligibilityReport =
            serde_json::from_value(value).expect("deserialize");
        assert_eq!(decoded, report);
        assert_eq!(decoded.run, run);
    }

    #[test]
    fn additive_run_detail_fields_remain_optional_for_v1_readers() {
        let mut value = serde_json::to_value(sample_run()).expect("serialize");
        let fields = value.as_object_mut().expect("run object");
        for field in [
            "phase",
            "blocker",
            "owner",
            "runtime",
            "provider",
            "heartbeat_at",
            "candidate",
            "gates",
            "publication",
            "action_eligibility",
        ] {
            fields.remove(field);
        }

        let decoded: ControlPlaneRun = serde_json::from_value(value).expect("older v1 run");
        assert!(decoded.action_eligibility.is_none());
        assert!(decoded.gates.is_empty());
    }

    #[test]
    fn result_envelope_versions_resources_once() {
        let ok = ControlPlaneResult::ok(sample_run());
        let value = serde_json::to_value(&ok).expect("serialize");
        assert_eq!(value["schema"], CONTROL_PLANE_RESULT_SCHEMA);
        assert_eq!(value["ok"], true);
        assert_eq!(value["resource"]["schema"], CONTROL_PLANE_RUN_SCHEMA);
        let decoded: ControlPlaneResult<ControlPlaneRun> =
            serde_json::from_value(value).expect("deserialize");
        assert_eq!(decoded, ok);

        let err = ControlPlaneResult::<ControlPlaneRun>::err(ControlPlaneError::not_found(
            "agent-task run not found: missing",
        ));
        let value = serde_json::to_value(&err).expect("serialize");
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"]["class"], "not_found");
        assert_eq!(value["error"]["retryable"], false);
        let decoded: ControlPlaneResult<ControlPlaneRun> =
            serde_json::from_value(value).expect("deserialize");
        assert_eq!(decoded, err);
        assert_eq!(
            decoded.error.as_ref().unwrap().class,
            ControlPlaneErrorClass::NotFound
        );
    }
}
