//! Versioned, runtime-neutral control-plane events and cursor pages.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::resource::ControlPlaneEvidenceRef;
use crate::{AttemptId, EventCursor, EventId, ExecutionId, MissionId, RunId, TaskId};

pub const CONTROL_PLANE_EVENT_SCHEMA: &str = "homeboy/control-plane-event/v1";
pub const CONTROL_PLANE_EVENT_PAGE_SCHEMA: &str = "homeboy/control-plane-event-page/v1";
pub const CONTROL_PLANE_EVENT_RETENTION_SCHEMA: &str = "homeboy/control-plane-event-retention/v1";
pub const CONTROL_PLANE_EVENT_APPEND_REQUEST_SCHEMA: &str =
    "homeboy/control-plane-event-append-request/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneEvent {
    pub schema: String,
    pub event: EventId,
    pub sequence: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub occurred_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mission: Option<MissionId>,
    pub run: RunId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<AttemptId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionId>,
    pub kind: String,
    pub source: ControlPlaneEventSource,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub data: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ControlPlaneEvidenceRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<ControlPlaneEvidenceRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneEventSource {
    pub component: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
}

/// Caller-owned event input. The service assigns the durable event identity,
/// sequence, and run identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneEventAppendRequest {
    pub schema: String,
    pub idempotency_key: String,
    pub actor: String,
    pub kind: String,
    pub source: ControlPlaneEventSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub occurred_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<AttemptId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionId>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub data: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ControlPlaneEvidenceRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<ControlPlaneEvidenceRef>,
}

impl ControlPlaneEventAppendRequest {
    pub fn validate(&self) -> Result<(), crate::ControlPlaneError> {
        if self.schema != CONTROL_PLANE_EVENT_APPEND_REQUEST_SCHEMA {
            return Err(crate::ControlPlaneError::invalid_argument(
                "unsupported control-plane event append request schema",
            ));
        }
        for (name, value, bound) in [
            ("idempotency_key", &self.idempotency_key, 256),
            ("actor", &self.actor, 256),
            ("kind", &self.kind, 128),
            ("source.component", &self.source.component, 128),
        ] {
            if value.trim().is_empty() || value.len() > bound {
                return Err(crate::ControlPlaneError::invalid_argument(format!(
                    "control-plane event append {name} is required and bounded"
                )));
            }
        }
        if self
            .source
            .instance
            .as_ref()
            .is_some_and(|value| value.len() > 256)
            || self
                .occurred_at
                .as_ref()
                .is_some_and(|value| value.len() > 128)
            || self.artifacts.len() > 32
            || self.evidence.len() > 32
            || serde_json::to_vec(&self.data).map_or(true, |value| value.len() > 64 * 1024)
        {
            return Err(crate::ControlPlaneError::invalid_argument(
                "control-plane event append input exceeds its bound",
            ));
        }
        for reference in self.artifacts.iter().chain(&self.evidence) {
            if reference.id.trim().is_empty()
                || reference.id.len() > 256
                || reference.kind.trim().is_empty()
                || reference.kind.len() > 128
                || reference.uri.trim().is_empty()
                || reference.uri.len() > 2_048
            {
                return Err(crate::ControlPlaneError::invalid_argument(
                    "control-plane event reference fields are required and bounded",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneEventPage {
    pub schema: String,
    pub run: RunId,
    pub events: Vec<ControlPlaneEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<EventCursor>,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneEventRetention {
    pub schema: String,
    pub run: RunId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub earliest_sequence: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_sequence: Option<u64>,
}

impl ControlPlaneEventPage {
    pub fn empty(run: RunId) -> Self {
        Self {
            schema: CONTROL_PLANE_EVENT_PAGE_SCHEMA.to_string(),
            run,
            events: Vec::new(),
            next_cursor: None,
            has_more: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn append_request() -> ControlPlaneEventAppendRequest {
        ControlPlaneEventAppendRequest {
            schema: CONTROL_PLANE_EVENT_APPEND_REQUEST_SCHEMA.to_string(),
            idempotency_key: "event-1".to_string(),
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
            data: Value::Null,
            artifacts: Vec::new(),
            evidence: Vec::new(),
        }
    }

    #[test]
    fn event_page_round_trips_with_typed_cursor_and_identities() {
        let run = RunId::new("run-1").expect("run");
        let page = ControlPlaneEventPage {
            schema: CONTROL_PLANE_EVENT_PAGE_SCHEMA.to_string(),
            run: run.clone(),
            events: vec![ControlPlaneEvent {
                schema: CONTROL_PLANE_EVENT_SCHEMA.to_string(),
                event: EventId::new("run-1:event:1").expect("event"),
                sequence: 1,
                occurred_at: Some("2026-01-01T00:00:00Z".to_string()),
                mission: None,
                run,
                task: Some(TaskId::new("task-1").expect("task")),
                attempt: None,
                execution: None,
                kind: "task.state_changed".to_string(),
                source: ControlPlaneEventSource {
                    component: "agent-task".to_string(),
                    instance: None,
                },
                data: serde_json::json!({ "state": "succeeded" }),
                artifacts: Vec::new(),
                evidence: Vec::new(),
            }],
            next_cursor: Some(EventCursor::new("1").expect("cursor")),
            has_more: false,
        };

        let value = serde_json::to_value(&page).expect("serialize");
        assert_eq!(value["schema"], CONTROL_PLANE_EVENT_PAGE_SCHEMA);
        assert_eq!(value["events"][0]["schema"], CONTROL_PLANE_EVENT_SCHEMA);
        assert_eq!(value["events"][0]["event"], "run-1:event:1");
        assert_eq!(value["next_cursor"], "1");
        let decoded: ControlPlaneEventPage = serde_json::from_value(value).expect("deserialize");
        assert_eq!(decoded, page);

        let retention = ControlPlaneEventRetention {
            schema: CONTROL_PLANE_EVENT_RETENTION_SCHEMA.to_string(),
            run: RunId::new("run-1").expect("run"),
            earliest_sequence: Some(1),
            latest_sequence: Some(10),
        };
        let value = serde_json::to_value(&retention).expect("serialize retention");
        assert_eq!(value["schema"], CONTROL_PLANE_EVENT_RETENTION_SCHEMA);
        assert_eq!(
            serde_json::from_value::<ControlPlaneEventRetention>(value)
                .expect("deserialize retention"),
            retention
        );
    }

    #[test]
    fn append_request_bounds_each_reference_field() {
        let mut request = append_request();
        request.evidence.push(ControlPlaneEvidenceRef {
            id: "evidence-1".to_string(),
            kind: "transcript".to_string(),
            uri: "homeboy://evidence/1".to_string(),
        });
        request.validate().expect("bounded reference");

        request.evidence[0].uri = "x".repeat(2_049);
        assert_eq!(
            request.validate().expect_err("oversized reference").class,
            crate::ControlPlaneErrorClass::InvalidArgument
        );
    }
}
