//! Shared imports, executors, and helpers for the `agent-task` command tests.
//!
//! Re-exports the command surface and fixtures so each concern submodule can
//! `use super::support::*` and stay focused on a single behavioral area.

pub(crate) use std::process::Command;

pub(in crate::commands::agent_task) use super::super::super::agent_task_dispatch::{
    DispatchArgs, DispatchCoreArgs,
};
pub(in crate::commands::agent_task) use super::super::args::{
    AgentTaskControllerApplyEventArgs, AgentTaskControllerFromSpecArgs,
    AgentTaskControllerMaterializeArgs,
};
pub(in crate::commands::agent_task) use super::super::args::{
    AgentTaskLoopArgs, CompileLoopArgs, ReviewArgs, StatusArgs, SubmitArgs, VerifyGateArgs,
};
pub(in crate::commands::agent_task) use super::super::controller::{
    apply_controller_event, controller_from_spec, controller_materialize,
    controller_run_action_with_executor, controller_run_next_with_executor,
    dispatch_args_from_controller_request,
};
pub(in crate::commands::agent_task) use super::super::run::{
    retry, run_loaded_plan, run_loop_with_executor, run_next_with_executor,
    run_resume_with_executor, run_submitted, submit,
};
pub(in crate::commands::agent_task) use super::super::status::{cancel, logs, status};
pub(in crate::commands::agent_task) use super::super::{
    review, CancelArgs, ProvidersArgs, RetryArgs,
};
pub(crate) use homeboy::core::agent_tasks::controller_service::{
    apply_spec_dispatch_defaults as apply_from_spec_dispatch_defaults,
    apply_spec_dispatch_defaults_with_cwd as apply_from_spec_dispatch_defaults_with_cwd,
};
pub(crate) use homeboy::core::agent_tasks::controller_service::{
    AgentTaskRepoLoopSpec, ControllerFromSpecRequest,
};
pub(crate) use homeboy::core::agent_tasks::gate::AgentTaskGateRevealPolicy;
pub(crate) use homeboy::core::agent_tasks::provider::ExtensionProviderAgentTaskExecutor;
pub(crate) use homeboy::core::agent_tasks::provider::{
    AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA, AGENT_TASK_PROVIDER_CAPABILITY_CONTRACT_SCHEMA,
};
pub(crate) use homeboy::core::agent_tasks::scheduler::{AgentTaskExecutorAdapter, AgentTaskPlan};

pub(crate) use crate::test_support::with_isolated_home;
pub(crate) use homeboy::core::agent_tasks::controller_service as agent_task_controller_service;
pub(crate) use homeboy::core::agent_tasks::lifecycle::{
    self as agent_task_lifecycle, status as lifecycle_status, AgentTaskRunRecord, AgentTaskRunState,
};
pub(crate) use homeboy::core::agent_tasks::loop_controller::{
    self as agent_task_loop_controller, AgentTaskLoopActionStatus, AgentTaskLoopPolicyAction,
};
pub(crate) use homeboy::core::agent_tasks::scheduler::{AgentTaskExecutionContext, AgentTaskState};
pub(crate) use homeboy::core::agent_tasks::{
    AgentTaskArtifact, AgentTaskDiagnostic, AgentTaskEvidenceRef, AgentTaskExecutor,
    AgentTaskFailureClassification, AgentTaskLimits, AgentTaskOutcome, AgentTaskOutcomeStatus,
    AgentTaskPolicy, AgentTaskRequest, AgentTaskWorkspace, AGENT_TASK_ARTIFACT_SCHEMA,
    AGENT_TASK_OUTCOME_SCHEMA, AGENT_TASK_REQUEST_SCHEMA,
};
pub(crate) use serde_json::{json, Value};
pub(crate) use std::sync::{Arc, Mutex};

pub(in crate::commands::agent_task) use super::super::contract;
pub(in crate::commands::agent_task) use super::super::loop_definition;
pub(in crate::commands::agent_task) use super::super::{ContractArgs, ContractFormat};

pub(crate) struct InspectingExecutor {
    pub(crate) run_id: String,
    pub(crate) observed_status: Arc<Mutex<Option<AgentTaskRunRecord>>>,
}

impl InspectingExecutor {
    pub(crate) fn noop(run_id: &str) -> Self {
        Self {
            run_id: run_id.to_string(),
            observed_status: Arc::new(Mutex::new(None)),
        }
    }
}

impl AgentTaskExecutorAdapter for InspectingExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        let record = lifecycle_status(&self.run_id).expect("status exists before executor runs");
        *self
            .observed_status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(record);

        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id,
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: Some("ok".to_string()),
            failure_classification: None,
            artifacts: Vec::new(),
            typed_artifacts: Vec::new(),
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct CapturingExecutor {
    pub(crate) observed_request: Arc<Mutex<Option<AgentTaskRequest>>>,
}

impl AgentTaskExecutorAdapter for CapturingExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        *self
            .observed_request
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(request.clone());

        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id,
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: Some("ok".to_string()),
            failure_classification: None,
            artifacts: Vec::new(),
            typed_artifacts: Vec::new(),
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        }
    }
}

pub(crate) struct DiagnosticFailureExecutor;

impl AgentTaskExecutorAdapter for DiagnosticFailureExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        AgentTaskOutcome {
                schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: request.task_id,
                status: AgentTaskOutcomeStatus::ProviderError,
                summary: Some("Embedded agent runtime failed.".to_string()),
                failure_classification: Some(AgentTaskFailureClassification::Provider),
                artifacts: Vec::new(),
                typed_artifacts: Vec::new(),
                evidence_refs: Vec::new(),
                diagnostics: vec![AgentTaskDiagnostic {
                    class: "provider_discovery".to_string(),
                    message: "Requested provider \"codex\" is not registered. Registered provider plugins: []"
                        .to_string(),
                    data: json!({ "registered_provider_plugins": [] }),
                }],
                outputs: Value::Null,
                workflow: None,
                follow_up: None,
                metadata: Value::Null,
            }
    }
}

pub(crate) struct ApplyArtifactExecutor;

impl AgentTaskExecutorAdapter for ApplyArtifactExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id,
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: Some("produced patch".to_string()),
            failure_classification: None,
            artifacts: vec![AgentTaskArtifact {
                schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                id: "patch-a".to_string(),
                kind: "patch".to_string(),
                name: Some("changes.patch".to_string()),
                label: None,
                role: None,
                semantic_key: None,
                path: Some("target/agent-task-review/changes.patch".to_string()),
                url: None,
                mime: Some("text/x-diff".to_string()),
                size_bytes: Some(42),
                sha256: Some("abc123".to_string()),
                metadata: Value::Null,
            }],
            typed_artifacts: Vec::new(),
            evidence_refs: vec![AgentTaskEvidenceRef {
                kind: "transcript".to_string(),
                uri: "target/agent-task-review/transcript.log".to_string(),
                label: Some("transcript".to_string()),
            }],
            diagnostics: Vec::new(),
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        }
    }
}

pub(crate) fn with_temp_home(run: impl FnOnce()) {
    with_isolated_home(|_| run());
}

pub(crate) fn test_plan() -> AgentTaskPlan {
    AgentTaskPlan::new(
        "plan-a",
        vec![AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "task-a".to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "test".to_string(),
                selector: Some("fixture".to_string()),
                runtime_selection: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model: None,
                config: Value::Null,
            },
            instructions: "run".to_string(),
            inputs: Value::Null,
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace::default(),
            component_contracts: Vec::new(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: Vec::new(),
            artifact_declarations: Vec::new(),
            metadata: Value::Null,
        }],
    )
}

pub(crate) fn agent_task_request_json(task_id: &str) -> Value {
    let mut plan = test_plan();
    let mut request = plan.tasks.pop().expect("test task");
    request.task_id = task_id.to_string();
    request.instructions = format!("run {task_id}");
    serde_json::to_value(request).expect("request json")
}
