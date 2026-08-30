//! Versioned, runtime-neutral control-plane events and cursor pages.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::resource::ControlPlaneEvidenceRef;
use crate::{AttemptId, EventCursor, EventId, ExecutionId, MissionId, RunId, TaskId};

pub const CONTROL_PLANE_EVENT_SCHEMA: &str = "homeboy/control-plane-event/v1";
pub const CONTROL_PLANE_EVENT_PAGE_SCHEMA: &str = "homeboy/control-plane-event-page/v1";

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
    }
}
