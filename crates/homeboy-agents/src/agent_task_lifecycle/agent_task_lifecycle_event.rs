use crate::agent_tasks::scheduler::AgentTaskAggregate;
use homeboy_core::api_jobs::{JobEvent, JobEventKind};
use homeboy_core::lab_contract::{
    AgentTaskDispatchIdentity, LabRunnerWorkload, LabRunnerWorkloadAgentTaskLifecycleMirrorPolicy,
};
use homeboy_core::{Error, Result};

pub(crate) const AGENT_TASK_RUN_PLAN_LIFECYCLE_EVENT_SCHEMA: &str =
    "homeboy/agent-task-run-plan-lifecycle-event/v1";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct AgentTaskRunPlanLifecycleEvent {
    #[serde(default = "agent_task_run_plan_lifecycle_event_schema")]
    pub schema: String,
    #[serde(default)]
    pub identity: AgentTaskDispatchIdentity,
    pub aggregate: AgentTaskAggregate,
}

fn agent_task_run_plan_lifecycle_event_schema() -> String {
    AGENT_TASK_RUN_PLAN_LIFECYCLE_EVENT_SCHEMA.to_string()
}

pub fn parse_offloaded_run_plan_envelope(stdout: &str) -> Result<serde_json::Value> {
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

pub fn is_agent_task_run_plan_envelope(value: &serde_json::Value) -> bool {
    let Some(data) = value.get("data") else {
        return false;
    };
    data.get("schema").and_then(serde_json::Value::as_str)
        == Some("homeboy/agent-task-aggregate/v1")
        || data.get("plan_id").is_some()
        || data.get("aggregate").is_some_and(|aggregate| {
            aggregate.get("schema").and_then(serde_json::Value::as_str)
                == Some("homeboy/agent-task-aggregate/v1")
                || aggregate.get("plan_id").is_some()
        })
}

pub fn agent_task_run_plan_lifecycle_event_from_job_events(
    job_events: Option<&[JobEvent]>,
) -> Option<AgentTaskRunPlanLifecycleEvent> {
    job_events?.iter().rev().find_map(|event| {
        if event.kind != JobEventKind::Result && event.kind != JobEventKind::Progress {
            return None;
        }
        agent_task_run_plan_lifecycle_event_from_value(event.data.as_ref()?)
    })
}

/// Rehydrates a typed lifecycle event from a persisted runner terminal result.
/// This is recovery-only: it uses the originally dispatched workload identity
/// and never starts provider execution.
pub(crate) fn agent_task_run_plan_lifecycle_event_from_persisted_job_events(
    job_events: &[JobEvent],
    runner_id: &str,
    runner_job_id: &str,
    persisted_run_id: &str,
) -> Result<Option<AgentTaskRunPlanLifecycleEvent>> {
    let Some(result) = job_events.iter().rev().find_map(|event| {
        (event.kind == JobEventKind::Result)
            .then_some(event.data.as_ref())
            .flatten()
    }) else {
        return Ok(None);
    };
    let workload = result
        .pointer("/data/runner_workload")
        .or_else(|| result.get("runner_workload"))
        .map(|value| serde_json::from_value::<LabRunnerWorkload>(value.clone()))
        .transpose()
        .map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("parse persisted Lab runner workload for agent-task recovery".to_string()),
            )
        })?;
    if let Some(event) = agent_task_run_plan_lifecycle_event_from_workload_result(
        workload.as_ref(),
        runner_id,
        runner_job_id,
        result,
    )? {
        return Ok(Some(event));
    }

    let Some(stdout) = result.get("stdout").and_then(serde_json::Value::as_str) else {
        return Ok(None);
    };
    let envelope = parse_offloaded_run_plan_envelope(stdout)?;
    let Some(data) = envelope.get("data") else {
        return Ok(None);
    };
    let Some(aggregate) = agent_task_aggregate_from_terminal_result(data)? else {
        return Ok(None);
    };
    Ok(Some(AgentTaskRunPlanLifecycleEvent {
        schema: AGENT_TASK_RUN_PLAN_LIFECYCLE_EVENT_SCHEMA.to_string(),
        identity: AgentTaskDispatchIdentity {
            runner_id: runner_id.to_string(),
            runner_job_id: runner_job_id.to_string(),
            persisted_run_id: Some(persisted_run_id.to_string()),
            run_id: Some(persisted_run_id.to_string()),
            ..AgentTaskDispatchIdentity::default()
        },
        aggregate,
    }))
}

pub fn agent_task_run_plan_lifecycle_event_from_value(
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

pub fn agent_task_run_plan_lifecycle_event_from_workload_result(
    workload: Option<&LabRunnerWorkload>,
    runner_id: &str,
    runner_job_id: &str,
    result: &serde_json::Value,
) -> Result<Option<AgentTaskRunPlanLifecycleEvent>> {
    let Some(agent_task) = workload.and_then(|workload| workload.agent_task.as_ref()) else {
        return Ok(None);
    };
    if agent_task.lifecycle_mirror_policy
        != LabRunnerWorkloadAgentTaskLifecycleMirrorPolicy::RunPlanAggregate
    {
        return Ok(None);
    }
    if let Some(event) = agent_task_run_plan_lifecycle_event_from_value(result) {
        return Ok(Some(event));
    }

    let Some(aggregate) = agent_task_aggregate_from_terminal_result(result)? else {
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

/// Remote runner results add a `data` envelope around the command result. Some
/// commands add their own `data` envelope, so inspect those result boundaries
/// instead of treating a valid aggregate as opaque terminal metadata.
fn agent_task_aggregate_from_terminal_result(
    value: &serde_json::Value,
) -> Result<Option<AgentTaskAggregate>> {
    const MAX_ENVELOPE_DEPTH: usize = 8;

    let mut current = value;
    for _ in 0..MAX_ENVELOPE_DEPTH {
        if let Some(aggregate) = agent_task_aggregate_from_value(current) {
            return Ok(Some(aggregate));
        }
        if current.get("schema").and_then(serde_json::Value::as_str)
            == Some("homeboy/agent-task-aggregate/v1")
            || current.get("plan_id").is_some()
        {
            return serde_json::from_value(current.clone())
                .map(Some)
                .map_err(|error| {
                    Error::internal_json(
                        error.to_string(),
                        Some("hydrate nested Lab terminal agent-task aggregate".to_string()),
                    )
                });
        }
        // Command results wrap their payload in `data`; agent-task cook then
        // wraps the terminal aggregate in its dispatch envelope's `aggregate`.
        let Some(next) = current.get("data").or_else(|| current.get("aggregate")) else {
            return Ok(None);
        };
        current = next;
    }

    Err(Error::internal_unexpected(
        "Lab terminal agent-task aggregate exceeded the supported result-envelope depth",
    ))
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

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_core::lab_contract::{
        LabRunnerWorkload, LabRunnerWorkloadAgentTask, LabRunnerWorkloadAgentTaskDispatchKind,
        LabRunnerWorkloadAgentTaskLifecycleMirrorPolicy,
    };

    fn workload() -> LabRunnerWorkload {
        serde_json::from_value(serde_json::json!({
            "schema": "homeboy/runner-workload/v1",
            "workload_id": "lab-agent-task",
            "kind": { "command_label": "agent-task", "command_family": "agent_task" },
            "agent_task": LabRunnerWorkloadAgentTask {
                run_id: "controller-run".to_string(),
                plan_ref: None,
                resolved_provider_policy: None,
                dispatch_kind: LabRunnerWorkloadAgentTaskDispatchKind::RunPlan,
                lifecycle_mirror_policy: LabRunnerWorkloadAgentTaskLifecycleMirrorPolicy::RunPlanAggregate,
            },
            "workspace_mappings": { "source_path_mode": "snapshot", "workspace_mode_policy": "snapshot", "mapping_ref": null },
            "required_capabilities": [],
            "required_secrets": { "categories": [], "secret_env_plan": {} },
            "required_extensions": [],
            "mutation_policy": { "capture_patch": true, "mutation_flag": null, "allow_dirty_lab_workspace": false },
            "assignment": { "runner_id": "homeboy-lab", "runner_mode": null, "source": null },
            "state": { "status": "submitted", "remote_workspace": null, "fallback_reason": null },
            "result_refs": { "plan_id": "lab-agent-task", "proof_id": null, "workspace_mapping_ref": null, "artifacts": [] }
        }))
        .expect("workload fixture")
    }

    #[test]
    fn hydrates_nested_terminal_aggregate_with_finalized_artifacts_and_provider_metadata() {
        let result = serde_json::json!({
            "exit_code": 0,
            "data": {
                "data": {
                    "schema": "homeboy/agent-task-aggregate/v1",
                    "plan_id": "plan-lab",
                    "status": "succeeded",
                    "totals": { "skipped": 0, "succeeded": 1 },
                    "outcomes": [{
                        "schema": "homeboy/agent-task-outcome/v1",
                        "task_id": "implement",
                        "status": "succeeded",
                        "artifacts": [{
                            "schema": "homeboy/agent-task-artifact/v1",
                            "id": "final-patch",
                            "kind": "patch",
                            "path": "artifacts/final.patch",
                            "size_bytes": 18928,
                            "sha256": "062f5c460c2dfb279277b75d5a16a04e3178ace1f35ce7b10da5e17441b37071",
                            "metadata": { "source_snapshot": "snapshot-1" }
                        }],
                        "typed_artifacts": [],
                        "evidence_refs": [{ "kind": "transcript", "uri": "homeboy://lab/transcript", "label": "Provider transcript" }],
                        "diagnostics": [],
                        "outputs": null,
                        "metadata": { "provider_run_id": "provider-run-1", "provider": "opencode" }
                    }]
                }
            }
        });

        let event = agent_task_run_plan_lifecycle_event_from_workload_result(
            Some(&workload()),
            "homeboy-lab",
            "runner-job-1",
            &result,
        )
        .expect("nested aggregate hydrates")
        .expect("lifecycle event");

        assert_eq!(event.identity.run_id.as_deref(), Some("controller-run"));
        assert_eq!(event.aggregate.outcomes[0].task_id, "implement");
        assert_eq!(
            event.aggregate.outcomes[0].artifacts[0].size_bytes,
            Some(18_928)
        );
        assert_eq!(
            event.aggregate.outcomes[0].artifacts[0].sha256.as_deref(),
            Some("062f5c460c2dfb279277b75d5a16a04e3178ace1f35ce7b10da5e17441b37071")
        );
        assert_eq!(
            event.aggregate.outcomes[0].metadata["provider_run_id"],
            "provider-run-1"
        );
    }

    #[test]
    fn rejects_malformed_nested_terminal_aggregate_with_hydration_context() {
        let result = serde_json::json!({
            "data": {
                "data": {
                    "schema": "homeboy/agent-task-aggregate/v1",
                    "plan_id": "plan-lab",
                    "status": "not-a-valid-status"
                }
            }
        });

        let error = agent_task_run_plan_lifecycle_event_from_workload_result(
            Some(&workload()),
            "homeboy-lab",
            "runner-job-1",
            &result,
        )
        .expect_err("malformed aggregate must fail hydration");

        assert_eq!(error.code, homeboy_core::ErrorCode::InternalJsonError);
        assert_eq!(
            error.details["context"],
            "hydrate nested Lab terminal agent-task aggregate"
        );
    }
}
