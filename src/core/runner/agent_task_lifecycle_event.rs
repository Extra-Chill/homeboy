use crate::core::agent_tasks::scheduler::AgentTaskAggregate;
use crate::core::api_jobs::{JobEvent, JobEventKind};
use crate::core::lab_contract::{
    AgentTaskDispatchIdentity, RunnerWorkload, RunnerWorkloadAgentTaskLifecycleMirrorPolicy,
};
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

pub(crate) fn agent_task_run_plan_lifecycle_event_from_job_events(
    job_events: Option<&[JobEvent]>,
) -> Option<AgentTaskRunPlanLifecycleEvent> {
    job_events?.iter().rev().find_map(|event| {
        if event.kind != JobEventKind::Result && event.kind != JobEventKind::Progress {
            return None;
        }
        agent_task_run_plan_lifecycle_event_from_value(event.data.as_ref()?)
    })
}

pub(crate) fn agent_task_run_plan_lifecycle_event_from_value(
    value: &serde_json::Value,
) -> Option<AgentTaskRunPlanLifecycleEvent> {
    if value.get("schema").and_then(serde_json::Value::as_str)
        == Some(AGENT_TASK_RUN_PLAN_LIFECYCLE_EVENT_SCHEMA)
    {
        return serde_json::from_value(value.clone()).ok();
    }
    if let Some(event) = value
        .get("agent_task_lifecycle_event")
        .and_then(agent_task_run_plan_lifecycle_event_from_value)
    {
        return Some(event);
    }
    value
        .get("data")
        .and_then(agent_task_run_plan_lifecycle_event_from_value)
}

pub(crate) fn agent_task_run_plan_lifecycle_event_from_workload_result(
    workload: Option<&RunnerWorkload>,
    runner_id: &str,
    runner_job_id: &str,
    result: &serde_json::Value,
) -> Result<Option<AgentTaskRunPlanLifecycleEvent>> {
    let Some(agent_task) = workload.and_then(|workload| workload.agent_task.as_ref()) else {
        return Ok(None);
    };
    if agent_task.lifecycle_mirror_policy
        != RunnerWorkloadAgentTaskLifecycleMirrorPolicy::RunPlanAggregate
    {
        return Ok(None);
    }
    if let Some(event) = agent_task_run_plan_lifecycle_event_from_value(result) {
        return Ok(Some(event));
    }

    let Some(aggregate) = result.get("data").and_then(agent_task_aggregate_from_value) else {
        return Ok(None);
    };

    Ok(Some(AgentTaskRunPlanLifecycleEvent {
        schema: AGENT_TASK_RUN_PLAN_LIFECYCLE_EVENT_SCHEMA.to_string(),
        identity: AgentTaskDispatchIdentity {
            runner_id: runner_id.to_string(),
            runner_job_id: runner_job_id.to_string(),
            persisted_run_id: Some(agent_task.run_id.clone()),
            run_id: Some(agent_task.run_id.clone()),
            ..AgentTaskDispatchIdentity::default()
        },
        aggregate,
    }))
}

fn agent_task_aggregate_from_value(value: &serde_json::Value) -> Option<AgentTaskAggregate> {
    if value.get("schema").and_then(serde_json::Value::as_str)
        == Some("homeboy/agent-task-aggregate/v1")
        || value.get("plan_id").is_some()
    {
        return serde_json::from_value(value.clone()).ok();
    }
    None
}
