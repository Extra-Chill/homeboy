//! Versioned control-plane resources and the shared result envelope.
//!
//! Resources and envelopes are versioned once. Callers do not mint a schema
//! per method.

use serde::{Deserialize, Serialize};

use crate::identity::{AttemptId, ExecutionId, MissionId, RunId};

pub const CONTROL_PLANE_RESULT_SCHEMA: &str = "homeboy/control-plane-result/v1";
pub const CONTROL_PLANE_RUN_SCHEMA: &str = "homeboy/control-plane-run/v1";

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
    pub message: String,
}

impl ControlPlaneError {
    pub fn not_found(message: impl Into<String>, next_action: impl Into<String>) -> Self {
        Self {
            class: ControlPlaneErrorClass::NotFound,
            retryable: false,
            next_action: Some(next_action.into()),
            message: message.into(),
        }
    }

    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self {
            class: ControlPlaneErrorClass::InvalidArgument,
            retryable: false,
            next_action: None,
            message: message.into(),
        }
    }

    pub fn unavailable(message: impl Into<String>, next_action: impl Into<String>) -> Self {
        Self {
            class: ControlPlaneErrorClass::Unavailable,
            retryable: true,
            next_action: Some(next_action.into()),
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
            run,
            attempt: None,
            attempt_number: None,
            state: ControlPlaneRunState::Unknown,
            location: None,
            execution: None,
            created_at: String::new(),
            updated_at: None,
            finished_at: None,
            evidence: Vec::new(),
            artifacts: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ControlPlaneRunState {
    Queued,
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

impl ControlPlaneRunState {
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Queued | Self::Running | Self::Unknown)
    }
}

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
        ControlPlaneError, ControlPlaneErrorClass, ControlPlaneEvidenceRef, ControlPlaneLocation,
        ControlPlaneResult, ControlPlaneRun, ControlPlaneRunState, CONTROL_PLANE_RESULT_SCHEMA,
        CONTROL_PLANE_RUN_SCHEMA,
    };
    use crate::{AttemptId, ExecutionId, MissionId, RunId};

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
        assert!(value.get("metadata").is_none());
        assert!(value.get("cwd").is_none());
        assert!(value.get("prompt").is_none());
        let decoded: ControlPlaneRun = serde_json::from_value(value).expect("deserialize");
        assert_eq!(decoded, resource);
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
            "homeboy agent-task active",
        ));
        let value = serde_json::to_value(&err).expect("serialize");
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"]["class"], "not_found");
        assert_eq!(value["error"]["retryable"], false);
        assert_eq!(value["error"]["next_action"], "homeboy agent-task active");
        let decoded: ControlPlaneResult<ControlPlaneRun> =
            serde_json::from_value(value).expect("deserialize");
        assert_eq!(decoded, err);
        assert_eq!(
            decoded.error.as_ref().unwrap().class,
            ControlPlaneErrorClass::NotFound
        );
    }
}
