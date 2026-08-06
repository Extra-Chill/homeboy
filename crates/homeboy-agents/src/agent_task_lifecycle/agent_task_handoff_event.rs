//! Typed dispatch-handoff event for agent-task cook/dispatch over Lab offload.
//!
//! Background (#7530). The run-plan half of the agent-task offload lifecycle is
//! already typed: the daemon emits a
//! `homeboy/agent-task-run-plan-lifecycle-event/v1` job event keyed by the
//! workload's `agent_task` section, and the controller mirrors the aggregate
//! from it (see [`super::agent_task_lifecycle_event`]).
//!
//! The cook/dispatch half was not. The controller recovered the handoff by
//! scanning the offloaded command's stdout, then its stderr, for any JSON
//! object that happened to carry a `homeboy/agent-task-lab-handoff/v1` or
//! `homeboy/agent-task-dispatch/v1` schema. That made failure-evidence
//! mirroring a function of what the remote command printed rather than of the
//! contract both sides already agreed on — a formatting change on the runner
//! silently degraded `agent-task status`/`logs` on the controller even though
//! the remote job itself had succeeded or failed cleanly.
//!
//! This module gives that half the same shape as the run-plan half:
//!
//! - The **runner/daemon** side extracts the handoff document once, on the side
//!   that owns the output, and only when
//!   [`LabRunnerWorkloadAgentTask::handoff_mirror_policy`] says this workload
//!   has one. It then republishes it as a schema-tagged job event carrying the
//!   run identity taken from the workload, not from the text.
//! - The **controller** side reads that typed event and never has to know the
//!   output format.
//!
//! The extraction itself is still a text scan today, because the current
//! `agent-task cook` binary prints its handoff to stdout. The win is that the
//! scan happens once, on the side that knows what it dispatched, gated by the
//! contract — and that a future runner can emit
//! [`AgentTaskDispatchHandoffEvent`] directly with no stdout involved and no
//! controller change. Until every runner does, the controller keeps its stdout
//! fallback; see `agent_task_bridge::resolve_offloaded_agent_task_handoff`.

use homeboy_core::api_jobs::{JobEvent, JobEventKind};
use homeboy_core::lab_contract::{
    AgentTaskDispatchIdentity, LabRunnerWorkload, LabRunnerWorkloadAgentTask,
    LabRunnerWorkloadAgentTaskHandoffMirrorPolicy,
};
use serde_json::Value;

/// Schema of the typed handoff document an agent-task cook/dispatch produces.
pub const AGENT_TASK_LAB_HANDOFF_SCHEMA: &str = "homeboy/agent-task-lab-handoff/v1";

/// Schema of the older dispatch envelope, still emitted by pre-typed binaries.
pub const AGENT_TASK_DISPATCH_ENVELOPE_SCHEMA: &str = "homeboy/agent-task-dispatch/v1";

/// Schema of [`AgentTaskDispatchHandoffEvent`] itself.
pub const AGENT_TASK_DISPATCH_HANDOFF_EVENT_SCHEMA: &str =
    "homeboy/agent-task-dispatch-handoff-event/v1";

/// Schema of the job-event wrapper the daemon appends. Mirrors the naming of
/// the run-plan wrapper (`homeboy/runner-workload-agent-task-lifecycle-event/v1`).
pub const AGENT_TASK_DISPATCH_HANDOFF_WORKLOAD_EVENT_SCHEMA: &str =
    "homeboy/runner-workload-agent-task-handoff-event/v1";

/// Key under which the typed event is attached to a job event / result payload.
pub const AGENT_TASK_DISPATCH_HANDOFF_EVENT_KEY: &str = "agent_task_dispatch_handoff_event";

/// A cook/dispatch handoff, bound to the run identity the workload declared.
///
/// `handoff` is deliberately kept as an opaque [`Value`]: the concrete handoff
/// struct lives in the runner crate, and this crate must not depend on it. What
/// this type adds over the raw document is the binding to `run_id`, which the
/// controller can check without trusting the document's own contents.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AgentTaskDispatchHandoffEvent {
    #[serde(default = "agent_task_dispatch_handoff_event_schema")]
    pub schema: String,
    #[serde(default)]
    pub identity: AgentTaskDispatchIdentity,
    /// The controller-owned agent-task run id, taken from the workload's
    /// `agent_task` section rather than parsed out of the handoff document.
    pub run_id: String,
    /// The handoff document, verbatim.
    pub handoff: Value,
}

fn agent_task_dispatch_handoff_event_schema() -> String {
    AGENT_TASK_DISPATCH_HANDOFF_EVENT_SCHEMA.to_string()
}

/// True when this workload declared that it produces a dispatch handoff.
///
/// A `None` policy means the emitting peer predates the field, which is exactly
/// the case where the consumer must fall back to output scraping.
pub fn agent_task_mirrors_dispatch_handoff(agent_task: &LabRunnerWorkloadAgentTask) -> bool {
    agent_task.handoff_mirror_policy
        == Some(LabRunnerWorkloadAgentTaskHandoffMirrorPolicy::DispatchHandoff)
}

/// Return `value` (or its `data` child) when it is a handoff document.
pub fn agent_task_handoff_document(value: &Value) -> Option<&Value> {
    fn is_handoff(value: &Value) -> bool {
        value
            .get("schema")
            .and_then(Value::as_str)
            .is_some_and(|schema| {
                schema == AGENT_TASK_LAB_HANDOFF_SCHEMA
                    || schema == AGENT_TASK_DISPATCH_ENVELOPE_SCHEMA
            })
    }

    if is_handoff(value) {
        return Some(value);
    }
    value.get("data").filter(|data| is_handoff(data))
}

/// Scan one output stream for a handoff document.
///
/// Mirrors the controller-side scanner's tolerance: the stream may be a single
/// JSON document, or arbitrary log lines with a JSON document embedded.
pub fn agent_task_handoff_document_from_output(output: &str) -> Option<Value> {
    if let Ok(value) = serde_json::from_str::<Value>(output) {
        if let Some(document) = agent_task_handoff_document(&value) {
            return Some(document.clone());
        }
    }

    for (index, _) in output.match_indices('{') {
        let mut stream = serde_json::Deserializer::from_str(&output[index..]).into_iter();
        if let Some(Ok(value)) = stream.next() {
            if let Some(document) = agent_task_handoff_document(&value) {
                return Some(document.clone());
            }
        }
    }

    None
}

/// Scan a terminal result payload for a handoff document: first the structured
/// result body, then its captured streams.
pub fn agent_task_handoff_document_from_result(result: &Value) -> Option<Value> {
    if let Some(document) = agent_task_handoff_document(result) {
        return Some(document.clone());
    }
    if let Some(data) = result.get("data") {
        if let Some(document) = agent_task_handoff_document(data) {
            return Some(document.clone());
        }
    }
    ["stdout", "stderr"]
        .into_iter()
        .filter_map(|field| {
            result
                .get(field)
                .or_else(|| result.pointer(&format!("/data/{field}")))
                .and_then(Value::as_str)
        })
        .find_map(agent_task_handoff_document_from_output)
}

/// Build the typed event from a runner terminal result, keyed by the workload
/// contract. Returns `None` when this workload declares no dispatch handoff,
/// which is the correct answer for run-plan and for every non-agent-task exec.
pub fn agent_task_dispatch_handoff_event_from_workload_result(
    workload: Option<&LabRunnerWorkload>,
    runner_id: &str,
    runner_job_id: &str,
    result: &Value,
) -> Option<AgentTaskDispatchHandoffEvent> {
    let agent_task = workload.and_then(|workload| workload.agent_task.as_ref())?;
    if !agent_task_mirrors_dispatch_handoff(agent_task) {
        return None;
    }
    if let Some(event) = agent_task_dispatch_handoff_event_from_value(result) {
        return Some(event);
    }
    let handoff = agent_task_handoff_document_from_result(result)?;
    Some(AgentTaskDispatchHandoffEvent {
        schema: AGENT_TASK_DISPATCH_HANDOFF_EVENT_SCHEMA.to_string(),
        identity: AgentTaskDispatchIdentity {
            runner_id: runner_id.to_string(),
            runner_job_id: runner_job_id.to_string(),
            persisted_run_id: Some(agent_task.run_id.clone()),
            run_id: Some(agent_task.run_id.clone()),
            ..AgentTaskDispatchIdentity::default()
        },
        run_id: agent_task.run_id.clone(),
        handoff,
    })
}

/// Recover the typed event from a job-event payload or a result envelope,
/// unwrapping the daemon's wrapper and any number of `data` layers.
pub fn agent_task_dispatch_handoff_event_from_value(
    value: &Value,
) -> Option<AgentTaskDispatchHandoffEvent> {
    if value.get("schema").and_then(Value::as_str) == Some(AGENT_TASK_DISPATCH_HANDOFF_EVENT_SCHEMA)
    {
        return serde_json::from_value(value.clone()).ok();
    }
    if let Some(event) = value
        .get(AGENT_TASK_DISPATCH_HANDOFF_EVENT_KEY)
        .and_then(agent_task_dispatch_handoff_event_from_value)
    {
        return Some(event);
    }
    value
        .get("data")
        .and_then(agent_task_dispatch_handoff_event_from_value)
}

/// Build the daemon's job-event payload for a typed handoff event.
///
/// The wrapper mirrors the run-plan lifecycle wrapper so both agent-task seams
/// look the same in a job-event stream. Constructed field by field rather than
/// through `json!` so the key constant is used literally by both the producer
/// here and [`agent_task_dispatch_handoff_event_from_value`].
pub fn agent_task_dispatch_handoff_workload_event_payload(
    event: &AgentTaskDispatchHandoffEvent,
) -> Value {
    let mut payload = serde_json::Map::new();
    payload.insert(
        "schema".to_string(),
        Value::String(AGENT_TASK_DISPATCH_HANDOFF_WORKLOAD_EVENT_SCHEMA.to_string()),
    );
    payload.insert(
        AGENT_TASK_DISPATCH_HANDOFF_EVENT_KEY.to_string(),
        serde_json::to_value(event).unwrap_or(Value::Null),
    );
    Value::Object(payload)
}

/// Controller-side entry point: find the most recent typed handoff event in a
/// runner job's event stream.
pub fn agent_task_dispatch_handoff_event_from_job_events(
    job_events: Option<&[JobEvent]>,
) -> Option<AgentTaskDispatchHandoffEvent> {
    job_events?.iter().rev().find_map(|event| {
        if event.kind != JobEventKind::Result && event.kind != JobEventKind::Progress {
            return None;
        }
        agent_task_dispatch_handoff_event_from_value(event.data.as_ref()?)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_core::lab_contract::{
        LabRunnerWorkloadAgentTaskDispatchKind, LabRunnerWorkloadAgentTaskLifecycleMirrorPolicy,
    };
    use serde_json::json;

    fn workload(agent_task: Option<serde_json::Value>) -> LabRunnerWorkload {
        let mut payload = json!({
            "schema": "homeboy/runner-workload/v1",
            "workload_id": "lab-agent-task",
            "kind": { "command_label": "agent-task cook", "command_family": "agent_task" },
            "workspace_mappings": { "source_path_mode": "snapshot", "workspace_mode_policy": "snapshot", "mapping_ref": null },
            "required_capabilities": [],
            "required_secrets": { "categories": [] },
            "required_extensions": [],
            "mutation_policy": { "capture_patch": true, "mutation_flag": null, "allow_dirty_lab_workspace": false },
            "assignment": { "runner_id": "homeboy-lab", "runner_mode": null, "source": null },
            "state": { "status": "submitted", "remote_workspace": null, "fallback_reason": null },
            "result_refs": { "plan_id": "lab-agent-task", "proof_id": null, "workspace_mapping_ref": null }
        });
        if let Some(agent_task) = agent_task {
            payload["agent_task"] = agent_task;
        }
        serde_json::from_value(payload).expect("workload fixture")
    }

    fn cook_workload() -> LabRunnerWorkload {
        workload(Some(json!({
            "run_id": "cook-7530",
            "dispatch_kind": "cook",
            "lifecycle_mirror_policy": "none",
            "handoff_mirror_policy": "dispatch_handoff"
        })))
    }

    fn handoff_document() -> serde_json::Value {
        json!({
            "schema": AGENT_TASK_LAB_HANDOFF_SCHEMA,
            "run_id": "cook-7530",
            "record_summary": {
                "run_id": "cook-7530",
                "plan_id": "plan-7530",
                "state": "failed",
                "task_count": 1
            }
        })
    }

    #[test]
    fn typed_event_is_keyed_by_the_workload_run_id_not_the_document() {
        // The document's own `run_id` is deliberately different: the workload
        // contract is authoritative for identity, output text is not.
        let mut document = handoff_document();
        document["run_id"] = json!("some-id-printed-by-the-remote-binary");
        let result = json!({ "exit_code": 1, "stdout": document.to_string(), "stderr": "" });

        let event = agent_task_dispatch_handoff_event_from_workload_result(
            Some(&cook_workload()),
            "homeboy-lab",
            "runner-job-1",
            &result,
        )
        .expect("cook handoff event");

        assert_eq!(event.run_id, "cook-7530");
        assert_eq!(event.identity.run_id.as_deref(), Some("cook-7530"));
        assert_eq!(event.identity.runner_id, "homeboy-lab");
        assert_eq!(
            event.handoff["run_id"],
            "some-id-printed-by-the-remote-binary"
        );
    }

    #[test]
    fn handoff_is_recovered_from_stderr_and_from_log_noise() {
        let noisy = format!(
            "HOMEBOY_RUNNER_PROGRESS {{\"phase\":\"finished\"}}\n{}\ntrailing log line\n",
            handoff_document()
        );
        let result = json!({ "exit_code": 1, "stdout": "", "stderr": noisy });

        let event = agent_task_dispatch_handoff_event_from_workload_result(
            Some(&cook_workload()),
            "homeboy-lab",
            "runner-job-1",
            &result,
        )
        .expect("handoff embedded in stderr log noise");

        assert_eq!(event.handoff["schema"], AGENT_TASK_LAB_HANDOFF_SCHEMA);
    }

    #[test]
    fn legacy_dispatch_envelope_is_still_recognized() {
        let envelope = json!({
            "schema": AGENT_TASK_DISPATCH_ENVELOPE_SCHEMA,
            "run_id": "cook-7530"
        });
        let result = json!({ "data": { "stdout": envelope.to_string() } });

        let event = agent_task_dispatch_handoff_event_from_workload_result(
            Some(&cook_workload()),
            "homeboy-lab",
            "runner-job-1",
            &result,
        )
        .expect("legacy dispatch envelope");

        assert_eq!(event.handoff["schema"], AGENT_TASK_DISPATCH_ENVELOPE_SCHEMA);
    }

    /// A workload whose emitter predates `handoff_mirror_policy` produces no
    /// typed event, so the consumer's retained stdout fallback is what runs.
    #[test]
    fn pre_typed_workload_emits_no_event() {
        let pre_typed = workload(Some(json!({
            "run_id": "cook-7530",
            "dispatch_kind": "cook",
            "lifecycle_mirror_policy": "none"
        })));
        let result = json!({ "stdout": handoff_document().to_string() });

        assert!(agent_task_dispatch_handoff_event_from_workload_result(
            Some(&pre_typed),
            "homeboy-lab",
            "runner-job-1",
            &result,
        )
        .is_none());
    }

    /// A run-plan declares `none`, and a generic exec has no `agent_task`
    /// section at all. Neither may produce a handoff event, no matter what its
    /// stdout happens to contain (the #9459 failure mode, one seam over).
    #[test]
    fn run_plan_and_generic_exec_emit_no_event() {
        let run_plan = workload(Some(json!({
            "run_id": "run-plan-7530",
            "plan_ref": "@plan.json",
            "dispatch_kind": "run_plan",
            "lifecycle_mirror_policy": "run_plan_aggregate",
            "handoff_mirror_policy": "none"
        })));
        let result = json!({ "stdout": handoff_document().to_string() });

        for candidate in [Some(run_plan), Some(workload(None)), None] {
            assert!(agent_task_dispatch_handoff_event_from_workload_result(
                candidate.as_ref(),
                "homeboy-lab",
                "runner-job-1",
                &result,
            )
            .is_none());
        }
    }

    #[test]
    fn plain_command_output_yields_no_handoff() {
        for stdout in [
            "/home/runner/workspace\n",
            "",
            "line one\nline two\n",
            "{ not valid json",
            r#"{"status":"fail","findings":3}"#,
        ] {
            let result = json!({ "stdout": stdout, "stderr": "" });
            assert!(
                agent_task_dispatch_handoff_event_from_workload_result(
                    Some(&cook_workload()),
                    "homeboy-lab",
                    "runner-job-1",
                    &result,
                )
                .is_none(),
                "plain stdout `{stdout:?}` must not look like a handoff"
            );
        }
    }

    #[test]
    fn event_round_trips_through_the_daemon_job_event_wrapper() {
        let event = agent_task_dispatch_handoff_event_from_workload_result(
            Some(&cook_workload()),
            "homeboy-lab",
            "runner-job-1",
            &json!({ "stdout": handoff_document().to_string() }),
        )
        .expect("cook handoff event");

        let job_event = JobEvent {
            sequence: 1,
            job_id: uuid::Uuid::nil(),
            kind: JobEventKind::Progress,
            timestamp_ms: 1,
            message: Some("agent-task dispatch handoff".to_string()),
            data: Some(agent_task_dispatch_handoff_workload_event_payload(&event)),
        };

        let recovered = agent_task_dispatch_handoff_event_from_job_events(Some(&[job_event]))
            .expect("typed event recovered from job events");
        assert_eq!(recovered.run_id, "cook-7530");
        assert_eq!(recovered.handoff["schema"], AGENT_TASK_LAB_HANDOFF_SCHEMA);
    }

    #[test]
    fn typed_helper_agrees_with_the_contract_spellings() {
        let agent_task = LabRunnerWorkloadAgentTask {
            run_id: "cook-7530".to_string(),
            plan_ref: None,
            resolved_provider_policy: None,
            dispatch_kind: LabRunnerWorkloadAgentTaskDispatchKind::Cook,
            lifecycle_mirror_policy: LabRunnerWorkloadAgentTaskLifecycleMirrorPolicy::None,
            handoff_mirror_policy: Some(
                LabRunnerWorkloadAgentTaskHandoffMirrorPolicy::DispatchHandoff,
            ),
        };
        assert!(agent_task_mirrors_dispatch_handoff(&agent_task));

        let no_handoff = LabRunnerWorkloadAgentTask {
            handoff_mirror_policy: Some(LabRunnerWorkloadAgentTaskHandoffMirrorPolicy::None),
            ..agent_task.clone()
        };
        assert!(!agent_task_mirrors_dispatch_handoff(&no_handoff));

        let pre_typed = LabRunnerWorkloadAgentTask {
            handoff_mirror_policy: None,
            ..agent_task
        };
        assert!(!agent_task_mirrors_dispatch_handoff(&pre_typed));
    }
}
