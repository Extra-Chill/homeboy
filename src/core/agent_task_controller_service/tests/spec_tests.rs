//! Controller record init/status/list + repo-loop spec round-trip tests.
use super::super::*;
use super::common::*;
use crate::core::agent_task::{
    AgentTaskArtifact, AgentTaskEvidenceRef, AgentTaskExecutor, AgentTaskLimits, AgentTaskOutcome,
    AgentTaskOutcomeStatus, AgentTaskPolicy, AgentTaskRequest, AgentTaskTypedArtifact,
    AgentTaskWorkspace, AGENT_TASK_ARTIFACT_SCHEMA, AGENT_TASK_OUTCOME_SCHEMA,
    AGENT_TASK_REQUEST_SCHEMA,
};
use crate::core::agent_task_loop_controller::{
    AgentTaskGateBundle, AgentTaskGateBundleCheck, AgentTaskGateBundleCheckKind,
    AgentTaskGateBundleStatus, AgentTaskLoopFindingPacket, AgentTaskLoopPolicyAction,
    AgentTaskLoopTerminalStatus, AgentTaskLoopWait, AgentTaskLoopWaitStatus,
    DEFAULT_FAN_OUT_MAX_ITEMS,
};
use crate::core::agent_task_scheduler::AgentTaskExecutionContext;
use crate::test_support::with_isolated_home;
use serde_json::json;
use std::sync::{Arc, Mutex};

#[test]
fn init_and_status_round_trip_controller_record() {
    with_isolated_home(|_| {
        let record = init(ControllerInitRequest {
            loop_id: "loop-service-init".to_string(),
            phase: "repair".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller initialized");

        assert_eq!(record.loop_id, "loop-service-init");
        assert_eq!(record.phase, "repair");

        let loaded = status("loop-service-init").expect("controller loaded");
        assert_eq!(loaded, record);
    });
}

#[test]
fn list_returns_existing_controllers() {
    with_isolated_home(|_| {
        init(ControllerInitRequest {
            loop_id: "loop-service-list-a".to_string(),
            phase: "init".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller a initialized");
        init(ControllerInitRequest {
            loop_id: "loop-service-list-b".to_string(),
            phase: "init".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller b initialized");

        let report = list().expect("controllers listed");
        assert_eq!(report.schema, LIST_RESULT_SCHEMA);
        assert_eq!(report.controllers.len(), 2);
    });
}

#[test]
fn repo_loop_spec_accepts_controller_id_and_keyed_contract_maps() {
    let spec: AgentTaskRepoLoopSpec = serde_json::from_value(json!({
        "schema": "homeboy/controller-spec/v1",
        "controller_id": "repo-loop-keyed-spec",
        "agents": {
            "repair-agent": {
                "role": "repair",
                "tools": ["repo-inspector"]
            }
        },
        "tools": {
            "repo-inspector": {
                "description": "inspect repo files"
            }
        },
        "workflows": {
            "repair-findings": {
                "agent_id": "repair-agent",
                "prompt": "Repair this finding.",
                "tools": ["repo-inspector"],
                "artifacts": ["patch"]
            }
        },
        "artifacts": {
            "patch": {
                "kind": "diff",
                "required": true
            }
        }
    }))
    .expect("keyed controller spec deserializes");

    assert_eq!(spec.loop_id, "repo-loop-keyed-spec");
    assert_eq!(spec.agents[0].agent_id, "repair-agent");
    assert_eq!(spec.tools[0].tool_id, "repo-inspector");
    assert_eq!(spec.workflows[0].workflow_id, "repair-findings");
    assert_eq!(spec.artifacts[0].artifact_id, "patch");
}

#[test]
fn repo_loop_spec_preserves_explicit_ids_inside_keyed_contract_maps() {
    let spec: AgentTaskRepoLoopSpec = serde_json::from_value(json!({
        "loop_id": "repo-loop-explicit-ids",
        "agents": {
            "repair": {
                "agent_id": "repair-agent"
            }
        },
        "tools": {
            "inspect": {
                "tool_id": "repo-inspector"
            }
        },
        "workflows": {
            "repair": {
                "workflow_id": "repair-findings",
                "prompt": "Repair this finding."
            }
        },
        "artifacts": {
            "patch-output": {
                "artifact_id": "patch",
                "kind": "diff"
            }
        }
    }))
    .expect("keyed controller spec deserializes");

    assert_eq!(spec.agents[0].agent_id, "repair-agent");
    assert_eq!(spec.tools[0].tool_id, "repo-inspector");
    assert_eq!(spec.workflows[0].workflow_id, "repair-findings");
    assert_eq!(spec.artifacts[0].artifact_id, "patch");
}
