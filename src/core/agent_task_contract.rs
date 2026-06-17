use serde::Serialize;

use crate::core::agent_task::{
    AgentTaskExecutionState, AgentTaskFailureClassification, AgentTaskOutcomeStatus,
    AgentTaskWorkflowStepStatus, AGENT_TASK_ARTIFACT_SCHEMA, AGENT_TASK_MATRIX_AGGREGATE_SCHEMA,
    AGENT_TASK_MATRIX_PLAN_SCHEMA, AGENT_TASK_OUTCOME_SCHEMA, AGENT_TASK_REQUEST_SCHEMA,
    AGENT_TASK_WORKFLOW_SCHEMA,
};
use crate::core::agent_task_aggregate::AGENT_TASK_AGGREGATE_SCHEMA;
use crate::core::agent_task_cook_loop::AGENT_TASK_COOK_LOOP_REPORT_SCHEMA;
use crate::core::agent_task_gate::AGENT_TASK_GATE_REPORT_SCHEMA;
use crate::core::agent_task_lifecycle::AgentTaskRunState;
use crate::core::agent_task_loop_controller::{
    AGENT_TASK_LOOP_CONTROLLER_SCHEMA, AGENT_TASK_LOOP_CONTROLLER_STATUS_SCHEMA,
};
use crate::core::agent_task_promotion::AGENT_TASK_PROMOTION_REPORT_SCHEMA;
use crate::core::agent_task_provider::{
    provider_capability_contract, AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA,
    AGENT_TASK_PROVIDER_CAPABILITY_CONTRACT_SCHEMA,
};
use crate::core::agent_task_schedule::{
    AgentTaskAggregateStatus, AgentTaskState, AGENT_TASK_PLAN_SCHEMA,
};
use crate::core::redaction::RedactionPolicy;
use crate::core::secret_env_plan::SECRET_ENV_PLAN_SCHEMA;

pub const AGENT_TASK_CORE_CONTRACT_SCHEMA: &str = "homeboy/agent-task-core-contract/v1";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentTaskCoreContract {
    pub schema: String,
    pub schemas: AgentTaskCoreContractSchemas,
    pub provider_capability: AgentTaskCoreProviderCapabilityContract,
    pub enums: AgentTaskCoreContractEnums,
    pub redaction_defaults: AgentTaskCoreRedactionDefaults,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentTaskCoreContractSchemas {
    pub request: String,
    pub outcome: String,
    pub artifact: String,
    pub workflow: String,
    pub plan: String,
    pub aggregate: String,
    pub matrix_plan: String,
    pub matrix_aggregate: String,
    pub provider: String,
    pub provider_capability_contract: String,
    pub gate_report: String,
    pub promotion_report: String,
    pub cook_loop_report: String,
    pub loop_controller: String,
    pub loop_controller_status: String,
    pub secret_env_plan: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentTaskCoreProviderCapabilityContract {
    pub schema: String,
    pub provider_schema: String,
    pub request_schema: String,
    pub outcome_schema: String,
    pub provider_capability_fields: Vec<String>,
    pub executor_provider_fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentTaskCoreContractEnums {
    pub execution_state: Vec<String>,
    pub task_state: Vec<String>,
    pub run_state: Vec<String>,
    pub aggregate_status: Vec<String>,
    pub outcome_status: Vec<String>,
    pub failure_classification: Vec<String>,
    pub workflow_step_status: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentTaskCoreRedactionDefaults {
    pub replacement: String,
    pub sensitive_keys: Vec<String>,
    pub sensitive_headers: Vec<String>,
}

pub fn agent_task_core_contract() -> AgentTaskCoreContract {
    let provider_capability = provider_capability_contract();
    let redaction = RedactionPolicy::default();

    AgentTaskCoreContract {
        schema: AGENT_TASK_CORE_CONTRACT_SCHEMA.to_string(),
        schemas: AgentTaskCoreContractSchemas {
            request: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            outcome: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            artifact: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
            workflow: AGENT_TASK_WORKFLOW_SCHEMA.to_string(),
            plan: AGENT_TASK_PLAN_SCHEMA.to_string(),
            aggregate: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
            matrix_plan: AGENT_TASK_MATRIX_PLAN_SCHEMA.to_string(),
            matrix_aggregate: AGENT_TASK_MATRIX_AGGREGATE_SCHEMA.to_string(),
            provider: AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA.to_string(),
            provider_capability_contract: AGENT_TASK_PROVIDER_CAPABILITY_CONTRACT_SCHEMA
                .to_string(),
            gate_report: AGENT_TASK_GATE_REPORT_SCHEMA.to_string(),
            promotion_report: AGENT_TASK_PROMOTION_REPORT_SCHEMA.to_string(),
            cook_loop_report: AGENT_TASK_COOK_LOOP_REPORT_SCHEMA.to_string(),
            loop_controller: AGENT_TASK_LOOP_CONTROLLER_SCHEMA.to_string(),
            loop_controller_status: AGENT_TASK_LOOP_CONTROLLER_STATUS_SCHEMA.to_string(),
            secret_env_plan: SECRET_ENV_PLAN_SCHEMA.to_string(),
        },
        provider_capability: AgentTaskCoreProviderCapabilityContract {
            schema: provider_capability.schema,
            provider_schema: provider_capability.provider_schema,
            request_schema: provider_capability.request_schema,
            outcome_schema: provider_capability.outcome_schema,
            provider_capability_fields: string_vec(&[
                "schema",
                "provider_schema",
                "request_schema",
                "outcome_schema",
            ]),
            executor_provider_fields: string_vec(&[
                "schema",
                "id",
                "label",
                "backend",
                "default_backend",
                "command",
                "request_schema",
                "outcome_schema",
                "capabilities",
                "secret_requirements",
                "secret_env_requirements",
                "workspace_materialization",
                "provider_defaults",
                "runner_readiness",
                "runner_sources",
                "dependency_failure_patterns",
                "timeout_artifact_discovery",
                "role_aliases",
                "extension_id",
                "extension_path",
                "runtime_id",
                "runtime_path",
                "extra",
            ]),
        },
        enums: AgentTaskCoreContractEnums {
            execution_state: enum_values(&[
                AgentTaskExecutionState::Queued,
                AgentTaskExecutionState::Running,
                AgentTaskExecutionState::Waiting,
                AgentTaskExecutionState::Succeeded,
                AgentTaskExecutionState::Failed,
                AgentTaskExecutionState::Cancelled,
            ]),
            task_state: enum_values(&[
                AgentTaskState::Queued,
                AgentTaskState::Blocked,
                AgentTaskState::Skipped,
                AgentTaskState::Running,
                AgentTaskState::Succeeded,
                AgentTaskState::Failed,
                AgentTaskState::Cancelled,
                AgentTaskState::TimedOut,
            ]),
            run_state: enum_values(&[
                AgentTaskRunState::Queued,
                AgentTaskRunState::Running,
                AgentTaskRunState::Succeeded,
                AgentTaskRunState::PartialFailure,
                AgentTaskRunState::Failed,
                AgentTaskRunState::Cancelled,
            ]),
            aggregate_status: enum_values(&[
                AgentTaskAggregateStatus::Succeeded,
                AgentTaskAggregateStatus::PartialFailure,
                AgentTaskAggregateStatus::Failed,
                AgentTaskAggregateStatus::Cancelled,
            ]),
            outcome_status: enum_values(&[
                AgentTaskOutcomeStatus::Succeeded,
                AgentTaskOutcomeStatus::NoOp,
                AgentTaskOutcomeStatus::UnableToRemediate,
                AgentTaskOutcomeStatus::ProviderError,
                AgentTaskOutcomeStatus::Timeout,
                AgentTaskOutcomeStatus::Failed,
                AgentTaskOutcomeStatus::FollowUpIssue,
                AgentTaskOutcomeStatus::Cancelled,
            ]),
            failure_classification: enum_values(&[
                AgentTaskFailureClassification::Provider,
                AgentTaskFailureClassification::Timeout,
                AgentTaskFailureClassification::PolicyDenied,
                AgentTaskFailureClassification::CapabilityMissing,
                AgentTaskFailureClassification::InvalidInput,
                AgentTaskFailureClassification::ExecutionFailed,
                AgentTaskFailureClassification::Unknown,
            ]),
            workflow_step_status: enum_values(&[
                AgentTaskWorkflowStepStatus::Pending,
                AgentTaskWorkflowStepStatus::Running,
                AgentTaskWorkflowStepStatus::Succeeded,
                AgentTaskWorkflowStepStatus::Failed,
                AgentTaskWorkflowStepStatus::Skipped,
                AgentTaskWorkflowStepStatus::Cancelled,
            ]),
        },
        redaction_defaults: AgentTaskCoreRedactionDefaults {
            replacement: redaction.replacement().to_string(),
            sensitive_keys: redaction.sensitive_keys().to_vec(),
            sensitive_headers: redaction.sensitive_headers().to_vec(),
        },
    }
}

fn string_vec(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

fn enum_values<T: Serialize>(variants: &[T]) -> Vec<String> {
    variants
        .iter()
        .map(|variant| {
            serde_json::to_value(variant)
                .ok()
                .and_then(|value| value.as_str().map(str::to_string))
                .unwrap_or_default()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_contract_exports_authoritative_agent_task_metadata() {
        let contract = agent_task_core_contract();

        assert_eq!(contract.schema, AGENT_TASK_CORE_CONTRACT_SCHEMA);
        assert_eq!(contract.schemas.request, AGENT_TASK_REQUEST_SCHEMA);
        assert_eq!(
            contract.schemas.provider,
            AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA
        );
        assert_eq!(
            contract.provider_capability.schema,
            AGENT_TASK_PROVIDER_CAPABILITY_CONTRACT_SCHEMA
        );
        assert!(contract
            .enums
            .outcome_status
            .contains(&"provider_error".to_string()));
        assert!(contract
            .enums
            .failure_classification
            .contains(&"capability_missing".to_string()));
        assert!(contract
            .redaction_defaults
            .sensitive_keys
            .contains(&"refresh_token".to_string()));
        assert!(contract
            .provider_capability
            .executor_provider_fields
            .contains(&"timeout_artifact_discovery".to_string()));
    }
}
