use crate::command_contract::AgentTaskDispatchIdentity;
use crate::core::agent_tasks::scheduler::AgentTaskAggregate;
use crate::core::{Error, Result};

pub(crate) const AGENT_TASK_RUN_PLAN_LIFECYCLE_EVENT_SCHEMA: &str =
    "homeboy/agent-task-run-plan-lifecycle-event/v1";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub(crate) struct AgentTaskRunPlanLifecycleEvent {
    #[serde(default = "agent_task_run_plan_lifecycle_event_schema")]
    pub schema: String,
    #[serde(default)]
    pub identity: AgentTaskDispatchIdentity,
    pub aggregate: AgentTaskAggregate,
}

fn agent_task_run_plan_lifecycle_event_schema() -> String {
    AGENT_TASK_RUN_PLAN_LIFECYCLE_EVENT_SCHEMA.to_string()
}

pub(crate) fn agent_task_run_plan_lifecycle_event_from_output(
    identity: AgentTaskDispatchIdentity,
    output: &str,
) -> Result<Option<AgentTaskRunPlanLifecycleEvent>> {
    let envelope = parse_offloaded_run_plan_envelope(output)?;
    if !is_agent_task_run_plan_envelope(&envelope) {
        return Ok(None);
    }
    let Some(aggregate_value) = envelope.get("data").cloned() else {
        return Ok(None);
    };
    let aggregate: AgentTaskAggregate =
        serde_json::from_value(aggregate_value).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("parse offloaded agent-task aggregate".to_string()),
            )
        })?;
    Ok(Some(AgentTaskRunPlanLifecycleEvent {
        schema: AGENT_TASK_RUN_PLAN_LIFECYCLE_EVENT_SCHEMA.to_string(),
        identity,
        aggregate,
    }))
}

pub(crate) fn parse_offloaded_run_plan_envelope(stdout: &str) -> Result<serde_json::Value> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(stdout) {
        return Ok(value);
    }

    let mut first_json = None;
    for (index, _) in stdout.match_indices('{') {
        let mut stream = serde_json::Deserializer::from_str(&stdout[index..]).into_iter();
        if let Some(Ok(value)) = stream.next() {
            if is_agent_task_run_plan_envelope(&value) {
                return Ok(value);
            }
            if first_json.is_none() {
                first_json = Some(value);
            }
        }
    }
    if let Some(value) = first_json {
        return Ok(value);
    }

    serde_json::from_str(stdout).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("parse offloaded agent-task run-plan output".to_string()),
        )
    })
}

pub(crate) fn is_agent_task_run_plan_envelope(value: &serde_json::Value) -> bool {
    value
        .get("data")
        .and_then(|data| data.get("schema"))
        .and_then(serde_json::Value::as_str)
        == Some("homeboy/agent-task-aggregate/v1")
        || value
            .get("data")
            .and_then(|data| data.get("plan_id"))
            .is_some()
}
