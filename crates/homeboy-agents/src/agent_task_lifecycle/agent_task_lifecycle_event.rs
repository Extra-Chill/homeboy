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
    // Recovery-only: this is reached for every terminal runner-exec result that
    // carries a `--run-id`, including generic (non-agent-task) execs whose
    // stdout is arbitrary command output (`pwd`, a fuzz report, empty, ...).
    // Plain command stdout is not an agent-task run-plan envelope, so a parse
    // failure here means "no agent-task aggregate to recover", not a hard error.
    // Only an actual run-plan envelope should produce a lifecycle event
    // (Extra-Chill/homeboy#9459).
    let Some(envelope) = parse_offloaded_run_plan_envelope(stdout)
        .ok()
        .filter(is_agent_task_run_plan_envelope)
    else {
        return Ok(None);
    };
    let Some(data) = envelope.get("data") else {
        return Ok(None);
    };
    let allow_compact_summary = workload
        .as_ref()
        .is_some_and(|workload| typed_run_plan_workload_matches(workload, persisted_run_id))
        || persisted_result_matches_run_plan(result, persisted_run_id);
    let Some(aggregate) = agent_task_aggregate_from_terminal_result(data, allow_compact_summary)?
    else {
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
    if !typed_run_plan_workload_matches_for_agent_task(agent_task, &agent_task.run_id) {
        return Ok(None);
    }
    if let Some(event) = agent_task_run_plan_lifecycle_event_from_value(result) {
        return Ok(Some(event));
    }

    let Some(aggregate) = agent_task_aggregate_from_terminal_result(result, true)? else {
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
    allow_compact_summary: bool,
) -> Result<Option<AgentTaskAggregate>> {
    const MAX_ENVELOPE_DEPTH: usize = 8;

    let mut current = value;
    for _ in 0..MAX_ENVELOPE_DEPTH {
        if current.get("view").and_then(serde_json::Value::as_str) == Some("summary") {
            if allow_compact_summary {
                if let Some(aggregate) = agent_task_aggregate_from_compact_summary(current)? {
                    return Ok(Some(aggregate));
                }
            } else {
                return Ok(None);
            }
        }
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

fn typed_run_plan_workload_matches(workload: &LabRunnerWorkload, run_id: &str) -> bool {
    workload.agent_task.as_ref().is_some_and(|agent_task| {
        typed_run_plan_workload_matches_for_agent_task(agent_task, run_id)
    })
}

fn typed_run_plan_workload_matches_for_agent_task(
    agent_task: &homeboy_core::lab_contract::LabRunnerWorkloadAgentTask,
    run_id: &str,
) -> bool {
    agent_task.run_id == run_id
        && agent_task.dispatch_kind
            == homeboy_core::lab_contract::LabRunnerWorkloadAgentTaskDispatchKind::RunPlan
        && agent_task.lifecycle_mirror_policy
            == LabRunnerWorkloadAgentTaskLifecycleMirrorPolicy::RunPlanAggregate
}

fn persisted_result_matches_run_plan(result: &serde_json::Value, run_id: &str) -> bool {
    let Some(command) = result
        .pointer("/data/command")
        .or_else(|| result.get("command"))
        .and_then(serde_json::Value::as_array)
    else {
        return false;
    };
    let command = command
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect::<Vec<_>>();
    command
        .windows(2)
        .any(|args| args == ["agent-task", "run-plan"])
        && command
            .windows(2)
            .any(|args| args[0] == "--record-run-id" && args[1] == run_id)
}

fn agent_task_aggregate_from_compact_summary(
    value: &serde_json::Value,
) -> Result<Option<AgentTaskAggregate>> {
    const COMPACT_REF_LIMIT: usize = 12;
    const COMPACT_TEXT_LIMIT: usize = 512;

    if value.get("schema").and_then(serde_json::Value::as_str)
        != Some("homeboy/agent-task-aggregate/v1")
        || value.get("view").and_then(serde_json::Value::as_str) != Some("summary")
    {
        return Ok(None);
    }
    if value
        .get("tasks_omitted")
        .and_then(serde_json::Value::as_u64)
        != Some(0)
    {
        return Err(Error::internal_unexpected(
            "cannot recover a truncated Lab terminal agent-task summary",
        ));
    }
    let Some(tasks) = value.get("tasks").and_then(serde_json::Value::as_array) else {
        return Ok(None);
    };
    for task in tasks {
        let refs_are_bounded = ["artifacts", "evidence_refs"].into_iter().all(|field| {
            task.get(field)
                .and_then(serde_json::Value::as_array)
                .is_none_or(|items| items.len() < COMPACT_REF_LIMIT)
        });
        let retained_text_is_bounded = task
            .get("summary")
            .and_then(serde_json::Value::as_str)
            .is_none_or(|text| text.chars().count() <= COMPACT_TEXT_LIMIT)
            && ["artifacts", "evidence_refs"].into_iter().all(|field| {
                task.get(field)
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .flat_map(|item| item.as_object().into_iter().flat_map(|item| item.values()))
                    .filter_map(serde_json::Value::as_str)
                    .all(|text| text.chars().count() <= COMPACT_TEXT_LIMIT)
            });
        if !refs_are_bounded || !retained_text_is_bounded {
            return Err(Error::internal_unexpected(
                "cannot recover a potentially truncated Lab terminal agent-task summary",
            ));
        }
    }
    let mut canonical = value.clone();
    let Some(canonical) = canonical.as_object_mut() else {
        return Ok(None);
    };
    canonical.insert(
        "outcomes".to_string(),
        serde_json::Value::Array(tasks.clone()),
    );
    for key in [
        "view",
        "tasks",
        "tasks_omitted",
        "failure_reasons",
        "run_id",
        "full_command",
        "evidence_command",
    ] {
        canonical.remove(key);
    }
    serde_json::from_value::<AgentTaskAggregate>(serde_json::Value::Object(canonical.clone()))
        .map(|mut aggregate| {
            for outcome in &mut aggregate.outcomes {
                outcome.metadata = serde_json::json!({
                    "terminal_recovery": "authenticated_compact_summary",
                });
            }
            Some(aggregate)
        })
        .map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("hydrate authenticated compact Lab terminal agent-task aggregate".to_string()),
            )
        })
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

    fn terminal_result_event(result: serde_json::Value) -> JobEvent {
        JobEvent {
            sequence: 1,
            job_id: uuid::Uuid::nil(),
            kind: JobEventKind::Result,
            timestamp_ms: 1,
            message: None,
            data: Some(result),
        }
    }

    #[test]
    fn generic_runner_exec_plain_stdout_recovers_no_agent_task_event() {
        // A generic `runner exec ... --run-id X -- pwd` terminal result carries
        // arbitrary command stdout and no agent-task workload. The recovery path
        // must NOT parse plain command output as an agent-task run-plan envelope
        // (Extra-Chill/homeboy#9459).
        for stdout in [
            "/home/runner/workspace\n",
            "",
            "line one\nline two\n",
            "{ not valid json",
        ] {
            let events = vec![terminal_result_event(serde_json::json!({
                "exit_code": 0,
                "stdout": stdout,
                "stderr": "",
            }))];

            let event = agent_task_run_plan_lifecycle_event_from_persisted_job_events(
                &events,
                "homeboy-lab",
                "runner-job-1",
                "generic-pwd",
            )
            .expect("generic runner exec stdout must not error");

            assert!(
                event.is_none(),
                "plain stdout `{stdout:?}` must not recover an agent-task lifecycle event"
            );
        }
    }

    #[test]
    fn generic_runner_exec_nonzero_exit_with_json_stdout_recovers_no_agent_task_event() {
        // A strict fuzz workload can emit a valid JSON result document to stdout
        // while intentionally exiting non-zero. That JSON is a command result,
        // not an agent-task run-plan envelope, so recovery still yields no event
        // and preserves the runner exit code path (Extra-Chill/homeboy#9459).
        let events = vec![terminal_result_event(serde_json::json!({
            "exit_code": 1,
            "stdout": r#"{"status":"fail","findings":3}"#,
            "stderr": "",
        }))];

        let event = agent_task_run_plan_lifecycle_event_from_persisted_job_events(
            &events,
            "homeboy-lab",
            "runner-job-1",
            "gutenberg-fuzz",
        )
        .expect("generic JSON stdout must not error");

        assert!(event.is_none());
    }

    #[test]
    fn persisted_run_plan_envelope_in_stdout_still_recovers_event() {
        // The recovery contract for real agent-task run-plan results is
        // preserved: an aggregate envelope embedded in stdout still hydrates.
        let events = vec![terminal_result_event(serde_json::json!({
            "exit_code": 0,
            "stdout": r#"{"data":{"schema":"homeboy/agent-task-aggregate/v1","plan_id":"plan-x","status":"succeeded","totals":{"skipped":0,"succeeded":0,"failed":0},"outcomes":[]}}"#,
            "stderr": "",
        }))];

        let event = agent_task_run_plan_lifecycle_event_from_persisted_job_events(
            &events,
            "homeboy-lab",
            "runner-job-1",
            "controller-run",
        )
        .expect("run-plan envelope recovery")
        .expect("agent-task lifecycle event");

        assert_eq!(event.identity.run_id.as_deref(), Some("controller-run"));
        assert_eq!(event.aggregate.plan_id, "plan-x");
    }

    #[test]
    fn persisted_command_result_stdout_recovers_authenticated_compact_aggregate() {
        let stdout = format!(
            "HOMEBOY_RUNNER_PROGRESS {{\"phase\":\"finished\"}}\n{}",
            serde_json::json!({
                "schema": "homeboy/command-result/v3",
                "command": "agent-task",
                "success": true,
                "exit_code": 0,
                "data": {
                    "schema": "homeboy/agent-task-aggregate/v1",
                    "view": "summary",
                    "plan_id": "plan-x",
                    "status": "succeeded",
                    "totals": { "skipped": 0, "succeeded": 1, "failed": 0 },
                    "tasks": [{
                        "task_id": "task-x",
                        "status": "succeeded",
                        "artifacts": [{
                            "schema": "homeboy/agent-task-artifact/v1",
                            "id": "patch-x",
                            "kind": "patch",
                            "size_bytes": 12704,
                            "sha256": "b86157f2c3735b453880c486455b263dfdbd8e77541cb5846b89754065fc9d9a"
                        }]
                    }],
                    "tasks_omitted": 0
                }
            })
        );
        let events = vec![terminal_result_event(serde_json::json!({
            "exit_code": 0,
            "command": [
                "homeboy", "agent-task", "run-plan", "--plan", "@plan.json",
                "--record-run-id", "cook-ssi-510-after-9849-v5-attempt-1-4f0b66a4"
            ],
            "stdout": stdout,
            "stderr": "",
        }))];

        let event = agent_task_run_plan_lifecycle_event_from_persisted_job_events(
            &events,
            "homeboy-lab",
            "fc3215cb-e657-485b-9887-96deaf0d5c5a",
            "cook-ssi-510-after-9849-v5-attempt-1-4f0b66a4",
        )
        .expect("command-result stdout recovery")
        .expect("agent-task lifecycle event");

        assert_eq!(event.aggregate.outcomes.len(), 1);
        let patch = &event.aggregate.outcomes[0].artifacts[0];
        assert_eq!(patch.size_bytes, Some(12_704));
        assert_eq!(
            patch.sha256.as_deref(),
            Some("b86157f2c3735b453880c486455b263dfdbd8e77541cb5846b89754065fc9d9a")
        );
    }

    #[test]
    fn persisted_compact_aggregate_rejects_mismatched_run_identity() {
        let stdout = serde_json::json!({
            "data": {
                "schema": "homeboy/agent-task-aggregate/v1",
                "view": "summary",
                "plan_id": "plan-x",
                "status": "succeeded",
                "totals": { "skipped": 0, "succeeded": 0, "failed": 0 },
                "tasks": [],
                "tasks_omitted": 0
            }
        })
        .to_string();
        let events = vec![terminal_result_event(serde_json::json!({
            "exit_code": 0,
            "command": [
                "homeboy", "agent-task", "run-plan", "--plan", "@plan.json",
                "--record-run-id", "another-run"
            ],
            "stdout": stdout,
            "stderr": "",
        }))];

        let event = agent_task_run_plan_lifecycle_event_from_persisted_job_events(
            &events,
            "homeboy-lab",
            "runner-job-1",
            "controller-run",
        )
        .expect("mismatched compact aggregate is ignored");

        assert!(event.is_none());
    }

    #[test]
    fn authenticated_compact_aggregate_rejects_ambiguous_reference_truncation() {
        let artifacts = (0..12)
            .map(|index| {
                serde_json::json!({
                    "id": format!("artifact-{index}"),
                    "kind": "patch",
                })
            })
            .collect::<Vec<_>>();
        let summary = serde_json::json!({
            "schema": "homeboy/agent-task-aggregate/v1",
            "view": "summary",
            "plan_id": "plan-x",
            "status": "succeeded",
            "totals": { "skipped": 0, "succeeded": 1, "failed": 0 },
            "tasks": [{
                "task_id": "task-x",
                "status": "succeeded",
                "artifacts": artifacts,
            }],
            "tasks_omitted": 0,
        });

        let error = agent_task_aggregate_from_terminal_result(&summary, true)
            .expect_err("a list at the compact cap may have omitted references");

        assert!(error.message.contains("potentially truncated"));
    }

    #[test]
    fn authenticated_compact_aggregate_rejects_bounded_reference_text() {
        let summary = serde_json::json!({
            "schema": "homeboy/agent-task-aggregate/v1",
            "view": "summary",
            "plan_id": "plan-x",
            "status": "succeeded",
            "totals": { "skipped": 0, "succeeded": 1, "failed": 0 },
            "tasks": [{
                "task_id": "task-x",
                "status": "succeeded",
                "artifacts": [{
                    "id": "patch",
                    "kind": "patch",
                    "url": format!("{}...", "x".repeat(512)),
                }],
            }],
            "tasks_omitted": 0,
        });

        let error = agent_task_aggregate_from_terminal_result(&summary, true)
            .expect_err("bounded artifact text is not authoritative evidence");

        assert!(error.message.contains("potentially truncated"));
    }
}
