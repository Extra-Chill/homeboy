//! compile_plan_from_spec and artifact-flow binding validation tests.
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
fn compile_plan_from_spec_derives_stage_dependencies_from_artifact_flow() {
    let mut spec = repo_loop_reconcile_spec("loop-plan-from-spec-flow");
    spec.workflows
        .iter_mut()
        .find(|workflow| workflow.workflow_id == "generation")
        .expect("generation workflow")
        .emits = vec!["static_site_pull_request".to_string()];
    let validation = spec
        .workflows
        .iter_mut()
        .find(|workflow| workflow.workflow_id == "static_validation")
        .expect("static validation workflow");
    validation.dependencies = Vec::new();
    validation.consumes = vec!["static_site_pull_request".to_string()];
    spec.artifact_graph = vec![AgentTaskRepoLoopSpecArtifactGraphEdge {
        artifact_id: "static_site_pull_request".to_string(),
        from_workflow_id: "generation".to_string(),
        to_workflow_id: "static_validation".to_string(),
        required: true,
    }];

    let report = compile_plan_from_spec(ControllerPlanRequest { spec }).expect("plan compiles");

    assert_eq!(report.schema, EXECUTABLE_PLAN_RESULT_SCHEMA);
    assert_eq!(report.loop_id, "loop-plan-from-spec-flow");
    assert!(report.spec_fingerprint.starts_with("sha256:"));
    assert_eq!(report.plan.kind, PlanKind::AgentTask);

    let generation = plan_stage(&report.plan, "stage:generation");
    assert!(generation.needs.is_empty());
    assert_eq!(
        generation.outputs["emits"],
        json!(["static_site_pull_request"])
    );

    let validation = plan_stage(&report.plan, "stage:static_validation");
    assert_eq!(validation.needs, vec!["stage:generation".to_string()]);
}

#[test]
fn compile_plan_from_spec_synthesizes_homeboy_runtime_artifact_stage() {
    let mut spec = repo_loop_reconcile_spec("loop-plan-from-spec-runtime");
    spec.workflows
        .iter_mut()
        .find(|workflow| workflow.workflow_id == "generation")
        .expect("generation workflow")
        .emits = vec!["static_site_pull_request".to_string()];
    let validation = spec
        .workflows
        .iter_mut()
        .find(|workflow| workflow.workflow_id == "static_validation")
        .expect("static validation workflow");
    validation.dependencies = Vec::new();
    validation.consumes = vec![
        "static_site_pull_request".to_string(),
        "static_validation_run".to_string(),
    ];
    // Homeboy-owned runtime artifact: no workflow produces it, kind ends in `_run`.
    spec.artifacts.push(AgentTaskRepoLoopSpecArtifact {
        artifact_id: "static_validation_run".to_string(),
        kind: "static_validation_run".to_string(),
        description: Some("Homeboy static validation run".to_string()),
        required: true,
    });
    spec.artifact_graph = vec![AgentTaskRepoLoopSpecArtifactGraphEdge {
        artifact_id: "static_site_pull_request".to_string(),
        from_workflow_id: "generation".to_string(),
        to_workflow_id: "static_validation".to_string(),
        required: true,
    }];

    let report = compile_plan_from_spec(ControllerPlanRequest { spec }).expect("plan compiles");

    assert_eq!(
        report.runtime_artifacts,
        vec!["static_validation_run".to_string()]
    );

    let runtime_stage = plan_stage(&report.plan, "runtime:static_validation_run");
    assert_eq!(runtime_stage.kind, "homeboy_runtime_artifact");
    assert_eq!(
        runtime_stage.inputs["runtime_artifact"]["owner"],
        json!("homeboy_runtime")
    );

    let validation = plan_stage(&report.plan, "stage:static_validation");
    assert!(validation.needs.contains(&"stage:generation".to_string()));
    assert!(validation
        .needs
        .contains(&"runtime:static_validation_run".to_string()));

    assert!(report
        .plan
        .artifacts
        .iter()
        .any(|artifact| artifact.id == "static_validation_run"));
}

#[test]
fn compile_plan_from_spec_rejects_unbacked_artifact_consumption() {
    let mut spec = repo_loop_reconcile_spec("loop-plan-from-spec-unbacked");
    let validation = spec
        .workflows
        .iter_mut()
        .find(|workflow| workflow.workflow_id == "static_validation")
        .expect("static validation workflow");
    validation.dependencies = Vec::new();
    // Consumes a repo artifact that no workflow emits and no artifact_flow edge backs.
    validation.consumes = vec!["static_site_pull_request".to_string()];

    let error = compile_plan_from_spec(ControllerPlanRequest { spec })
        .expect_err("unbacked consumption rejected");
    let message = error.to_string();
    assert!(
        message.contains("artifact_flow") || message.contains("static_site_pull_request"),
        "unexpected error: {message}"
    );
}

#[test]
fn validate_artifact_flow_bindings_accepts_emit_consume_pairing() {
    let mut spec = repo_loop_reconcile_spec("loop-flow-bindings");
    spec.workflows
        .iter_mut()
        .find(|workflow| workflow.workflow_id == "generation")
        .expect("generation workflow")
        .emits = vec!["static_site_pull_request".to_string()];
    let validation = spec
        .workflows
        .iter_mut()
        .find(|workflow| workflow.workflow_id == "static_validation")
        .expect("static validation workflow");
    validation.dependencies = Vec::new();
    validation.consumes = vec!["static_site_pull_request".to_string()];

    validate_artifact_flow_bindings(&spec).expect("emit/consume pairing is valid");
}
