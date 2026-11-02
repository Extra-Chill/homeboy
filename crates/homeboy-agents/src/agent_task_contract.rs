use serde::Serialize;

use crate::agent_task::{
    AgentTaskExecutionState, AgentTaskFailureClassification, AgentTaskOutcomeStatus,
    AgentTaskWorkflowStepStatus, AGENT_COMMAND_POLICY_SCHEMA, AGENT_TASK_ARTIFACT_SCHEMA,
    AGENT_TASK_MATRIX_AGGREGATE_SCHEMA, AGENT_TASK_MATRIX_PLAN_SCHEMA, AGENT_TASK_OUTCOME_SCHEMA,
    AGENT_TASK_REQUEST_SCHEMA, AGENT_TASK_WORKFLOW_SCHEMA, AGENT_TOOL_POLICY_SCHEMA,
    AGENT_TOOL_REQUEST_SCHEMA, AGENT_TOOL_RESULT_SCHEMA,
};
use crate::agent_task_aggregate::AGENT_TASK_AGGREGATE_SCHEMA;
use crate::agent_task_cook_loop::AGENT_TASK_COOK_FEEDBACK_REPORT_SCHEMA;
use crate::agent_task_gate::AGENT_TASK_GATE_REPORT_SCHEMA;
use crate::agent_task_lifecycle::AgentTaskRunState;
use crate::agent_task_promotion::AGENT_TASK_PROMOTION_REPORT_SCHEMA;
use crate::agent_task_provider::{
    provider_capability_contract, ProviderSchemaContract, AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA,
    AGENT_TASK_PROVIDER_CAPABILITY_CONTRACT_SCHEMA,
    RESOLVED_AGENT_RUNTIME_EXECUTION_CONTRACT_SCHEMA,
};
use crate::agent_task_schedule::{AgentTaskManagedService, AgentTaskState, AGENT_TASK_PLAN_SCHEMA};
use crate::agent_tool_control_plane::AGENT_TOOL_DISPATCH_EVIDENCE_SCHEMA;
use homeboy_core::command_invocation::COMMAND_INVOCATION_SCHEMA;
use homeboy_core::redaction::RedactionPolicy;
use homeboy_core::secret_env_plan::SECRET_ENV_PLAN_SCHEMA;

pub const AGENT_TASK_CORE_CONTRACT_SCHEMA: &str = "homeboy/agent-task-core-contract/v1";
pub const AGENT_TASK_ARTIFACT_DECLARATION_SCHEMA: &str =
    "homeboy/agent-task-artifact-declaration/v1";
pub const AGENT_TASK_ARTIFACT_HANDOFF_SCHEMA: &str = "homeboy/agent-task-artifact-handoff/v1";
pub const AGENT_TASK_EVIDENCE_REF_SCHEMA: &str = "homeboy/agent-task-evidence-ref/v1";
pub const AGENT_TASK_PROVIDER_OUTCOME_CONTRACT_SCHEMA: &str =
    "homeboy/agent-task-provider-outcome-contract/v1";
pub const AGENT_TASK_DIAGNOSTIC_RANKING_SCHEMA: &str = "homeboy/agent-task-diagnostic-ranking/v1";
pub const SECRET_ENV_REQUIREMENT_SCHEMA: &str = "homeboy/secret-env-requirement/v1";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentTaskCoreContract {
    pub schema: String,
    pub schemas: AgentTaskCoreContractSchemas,
    pub provider_capability: AgentTaskCoreProviderCapabilityContract,
    pub managed_services: AgentTaskManagedServiceContract,
    pub enums: AgentTaskCoreContractEnums,
    pub redaction_defaults: AgentTaskCoreRedactionDefaults,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentTaskManagedServiceContract {
    pub direct_plan_field: String,
    pub durable_plan_path: String,
    pub version: u32,
    pub fields: Vec<String>,
    pub lifecycle_values: Vec<String>,
    pub default_cleanup_deadline_ms: u64,
    pub max_cleanup_deadline_ms: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentTaskCoreContractSchemas {
    pub request: String,
    pub outcome: String,
    pub artifact: String,
    pub artifact_declaration: String,
    pub artifact_handoff: String,
    pub evidence_ref: String,
    pub provider_outcome_contract: String,
    pub diagnostic_ranking: String,
    pub workflow: String,
    pub plan: String,
    pub plan_validation: String,
    pub control_plane_action_eligibility: String,
    pub aggregate: String,
    pub matrix_plan: String,
    pub matrix_aggregate: String,
    pub provider: String,
    pub provider_capability_contract: String,
    pub tool_request: String,
    pub tool_result: String,
    pub tool_policy: String,
    /// Declarative command policy carried inside the tool policy. A runtime
    /// that executes shell commands in its own process is expected to honour
    /// this rather than leaving resource safety to prompt text (#11481).
    pub command_policy: String,
    pub tool_dispatch_evidence: String,
    pub gate_report: String,
    pub promotion_report: String,
    pub cook_feedback_report: String,
    pub secret_env_plan: String,
    pub secret_env_requirement: String,
    pub command_invocation: String,
    pub resolved_agent_runtime_execution_contract: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentTaskCoreProviderCapabilityContract {
    pub schema: String,
    pub provider_schema: String,
    #[serde(flatten)]
    pub schemas: ProviderSchemaContract,
    pub request_required_fields: Vec<String>,
    pub outcome_statuses: Vec<String>,
    pub failure_classifications: Vec<String>,
    pub redacted_metadata_keys: Vec<String>,
    pub tool_request_schema: String,
    pub tool_result_schema: String,
    pub tool_policy_schema: String,
    pub provider_capability_fields: Vec<String>,
    pub executor_provider_fields: Vec<String>,
    pub workspace_materialization_fields: Vec<String>,
    pub workspace_mount_spec_fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentTaskCoreContractEnums {
    pub execution_handle_kind: Vec<String>,
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
            artifact_declaration: AGENT_TASK_ARTIFACT_DECLARATION_SCHEMA.to_string(),
            artifact_handoff: AGENT_TASK_ARTIFACT_HANDOFF_SCHEMA.to_string(),
            evidence_ref: AGENT_TASK_EVIDENCE_REF_SCHEMA.to_string(),
            provider_outcome_contract: AGENT_TASK_PROVIDER_OUTCOME_CONTRACT_SCHEMA.to_string(),
            diagnostic_ranking: AGENT_TASK_DIAGNOSTIC_RANKING_SCHEMA.to_string(),
            workflow: AGENT_TASK_WORKFLOW_SCHEMA.to_string(),
            plan: AGENT_TASK_PLAN_SCHEMA.to_string(),
            plan_validation: crate::agent_task_service::AGENT_TASK_PLAN_VALIDATION_SCHEMA
                .to_string(),
            control_plane_action_eligibility:
                homeboy_control_plane_contract::CONTROL_PLANE_ACTION_ELIGIBILITY_SCHEMA.to_string(),
            aggregate: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
            matrix_plan: AGENT_TASK_MATRIX_PLAN_SCHEMA.to_string(),
            matrix_aggregate: AGENT_TASK_MATRIX_AGGREGATE_SCHEMA.to_string(),
            provider: AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA.to_string(),
            provider_capability_contract: AGENT_TASK_PROVIDER_CAPABILITY_CONTRACT_SCHEMA
                .to_string(),
            tool_request: AGENT_TOOL_REQUEST_SCHEMA.to_string(),
            tool_result: AGENT_TOOL_RESULT_SCHEMA.to_string(),
            tool_policy: AGENT_TOOL_POLICY_SCHEMA.to_string(),
            command_policy: AGENT_COMMAND_POLICY_SCHEMA.to_string(),
            tool_dispatch_evidence: AGENT_TOOL_DISPATCH_EVIDENCE_SCHEMA.to_string(),
            gate_report: AGENT_TASK_GATE_REPORT_SCHEMA.to_string(),
            promotion_report: AGENT_TASK_PROMOTION_REPORT_SCHEMA.to_string(),
            cook_feedback_report: AGENT_TASK_COOK_FEEDBACK_REPORT_SCHEMA.to_string(),
            secret_env_plan: SECRET_ENV_PLAN_SCHEMA.to_string(),
            secret_env_requirement: SECRET_ENV_REQUIREMENT_SCHEMA.to_string(),
            command_invocation: COMMAND_INVOCATION_SCHEMA.to_string(),
            resolved_agent_runtime_execution_contract:
                RESOLVED_AGENT_RUNTIME_EXECUTION_CONTRACT_SCHEMA.to_string(),
        },
        provider_capability: AgentTaskCoreProviderCapabilityContract {
            schema: provider_capability.schema,
            provider_schema: provider_capability.provider_schema,
            schemas: ProviderSchemaContract {
                request_schema: provider_capability.schemas.request_schema,
                outcome_schema: provider_capability.schemas.outcome_schema,
            },
            request_required_fields: agent_task_request_required_fields(),
            outcome_statuses: agent_task_outcome_statuses(),
            failure_classifications: agent_task_failure_classifications(),
            redacted_metadata_keys: agent_task_redacted_metadata_keys(),
            tool_request_schema: provider_capability.tool_request_schema,
            tool_result_schema: provider_capability.tool_result_schema,
            tool_policy_schema: provider_capability.tool_policy_schema,
            provider_capability_fields: string_vec(&[
                "schema",
                "provider_schema",
                "request_schema",
                "outcome_schema",
                "request_required_fields",
                "outcome_statuses",
                "failure_classifications",
                "redacted_metadata_keys",
                "tool_request_schema",
                "tool_result_schema",
                "tool_policy_schema",
            ]),
            executor_provider_fields: string_vec(&[
                "schema",
                "id",
                "label",
                "backend",
                "default_backend",
                "command",
                "command_argv",
                "invocation",
                "request_schema",
                "outcome_schema",
                "capabilities",
                "workspace_materialization",
                "provider_defaults",
                "provider_preflight",
                "artifact_contract",
                "extension_id",
                "extension_path",
                "runtime_id",
                "runtime_path",
            ]),
            workspace_materialization_fields: string_vec(&[
                "cwd",
                "requires_git",
                "write_scope",
                "artifact_paths",
                "spec",
                "mounts",
                "apply_back",
                "extra",
            ]),
            workspace_mount_spec_fields: string_vec(&[
                "handle",
                "repo",
                "host_path",
                "target_path",
                "mode",
                "materialization",
                "metadata",
                "extra",
            ]),
        },
        managed_services: AgentTaskManagedServiceContract {
            direct_plan_field: "services".to_string(),
            durable_plan_path: "inputs.managed_services".to_string(),
            version: AgentTaskManagedService::VERSION,
            fields: string_vec(AgentTaskManagedService::FIELDS),
            lifecycle_values: string_vec(
                crate::agent_task_schedule::AgentTaskManagedServiceLifecycle::VALUES,
            ),
            default_cleanup_deadline_ms: AgentTaskManagedService::DEFAULT_CLEANUP_DEADLINE_MS,
            max_cleanup_deadline_ms: AgentTaskManagedService::MAX_CLEANUP_DEADLINE_MS,
        },
        enums: AgentTaskCoreContractEnums {
            execution_handle_kind: string_vec(&[
                "queued_record",
                "local_pid",
                "runner_job",
                "provider_run",
            ]),
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
                AgentTaskState::CandidateRecoverable,
            ]),
            run_state: enum_values(&RUN_STATES),
            // An aggregate's status is the terminal run state it derives
            // (`AgentTaskRunState::from_outcomes`).
            aggregate_status: enum_values(
                &RUN_STATES
                    .into_iter()
                    .filter(|state| state.is_terminal())
                    .collect::<Vec<_>>(),
            ),
            outcome_status: agent_task_outcome_statuses(),
            failure_classification: agent_task_failure_classifications(),
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

fn agent_task_request_required_fields() -> Vec<String> {
    string_vec(&["schema", "task_id", "executor.backend", "instructions"])
}

fn agent_task_outcome_statuses() -> Vec<String> {
    enum_values(&[
        AgentTaskOutcomeStatus::Succeeded,
        AgentTaskOutcomeStatus::NoOp,
        AgentTaskOutcomeStatus::UnableToRemediate,
        AgentTaskOutcomeStatus::ProviderError,
        AgentTaskOutcomeStatus::Timeout,
        AgentTaskOutcomeStatus::CandidateRecoverable,
        AgentTaskOutcomeStatus::Failed,
        AgentTaskOutcomeStatus::FollowUpIssue,
        AgentTaskOutcomeStatus::Cancelled,
    ])
}

fn agent_task_failure_classifications() -> Vec<String> {
    enum_values(&[
        AgentTaskFailureClassification::Provider,
        AgentTaskFailureClassification::Transient,
        AgentTaskFailureClassification::Timeout,
        AgentTaskFailureClassification::Stalled,
        AgentTaskFailureClassification::RateLimited,
        AgentTaskFailureClassification::ProviderAccountBlocked,
        AgentTaskFailureClassification::ProviderQuotaExhausted,
        AgentTaskFailureClassification::ProviderBillingBlocked,
        AgentTaskFailureClassification::ProviderCredentialsExhausted,
        AgentTaskFailureClassification::PolicyDenied,
        AgentTaskFailureClassification::CapabilityMissing,
        AgentTaskFailureClassification::Capacity,
        AgentTaskFailureClassification::InvalidInput,
        AgentTaskFailureClassification::ExecutionFailed,
        AgentTaskFailureClassification::Unknown,
    ])
}

fn agent_task_redacted_metadata_keys() -> Vec<String> {
    string_vec(&["secret_env_values", "secretEnvValues", "secrets"])
}

fn string_vec(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

const RUN_STATES: [AgentTaskRunState; 7] = [
    AgentTaskRunState::Queued,
    AgentTaskRunState::Running,
    AgentTaskRunState::Succeeded,
    AgentTaskRunState::CandidateRecoverable,
    AgentTaskRunState::PartialFailure,
    AgentTaskRunState::Failed,
    AgentTaskRunState::Cancelled,
];

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
            contract.schemas.plan_validation,
            crate::agent_task_service::AGENT_TASK_PLAN_VALIDATION_SCHEMA
        );
        assert_eq!(
            contract.schemas.control_plane_action_eligibility,
            homeboy_control_plane_contract::CONTROL_PLANE_ACTION_ELIGIBILITY_SCHEMA
        );
        assert_eq!(
            contract.schemas.provider,
            AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA
        );
        assert_eq!(
            contract.provider_capability.schema,
            AGENT_TASK_PROVIDER_CAPABILITY_CONTRACT_SCHEMA
        );
        assert_eq!(
            contract.schemas.artifact_declaration,
            AGENT_TASK_ARTIFACT_DECLARATION_SCHEMA
        );
        assert_eq!(
            contract.schemas.evidence_ref,
            AGENT_TASK_EVIDENCE_REF_SCHEMA
        );
        assert_eq!(
            contract.schemas.secret_env_requirement,
            SECRET_ENV_REQUIREMENT_SCHEMA
        );
        assert_eq!(
            contract.schemas.command_invocation,
            COMMAND_INVOCATION_SCHEMA
        );
        assert_eq!(
            contract.schemas.resolved_agent_runtime_execution_contract,
            RESOLVED_AGENT_RUNTIME_EXECUTION_CONTRACT_SCHEMA
        );
        assert_eq!(
            contract.provider_capability.request_required_fields,
            agent_task_request_required_fields()
        );
        assert_eq!(
            contract.provider_capability.outcome_statuses,
            agent_task_outcome_statuses()
        );
        assert_eq!(
            contract.provider_capability.failure_classifications,
            agent_task_failure_classifications()
        );
        assert_eq!(
            contract.provider_capability.redacted_metadata_keys,
            agent_task_redacted_metadata_keys()
        );
        assert!(contract
            .provider_capability
            .executor_provider_fields
            .contains(&"invocation".to_string()));
        assert_eq!(contract.schemas.tool_request, AGENT_TOOL_REQUEST_SCHEMA);
        assert_eq!(contract.schemas.tool_result, AGENT_TOOL_RESULT_SCHEMA);
        assert_eq!(contract.schemas.tool_policy, AGENT_TOOL_POLICY_SCHEMA);
        assert_eq!(contract.schemas.command_policy, AGENT_COMMAND_POLICY_SCHEMA);
        assert_eq!(
            contract.provider_capability.tool_request_schema,
            AGENT_TOOL_REQUEST_SCHEMA
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
            .workspace_materialization_fields
            .contains(&"mounts".to_string()));
        assert!(contract
            .provider_capability
            .workspace_mount_spec_fields
            .contains(&"target_path".to_string()));
        assert_eq!(contract.managed_services.direct_plan_field, "services");
        assert_eq!(
            contract.managed_services.durable_plan_path,
            "inputs.managed_services"
        );
        assert_eq!(
            contract.managed_services.version,
            AgentTaskManagedService::VERSION
        );
        assert!(contract
            .managed_services
            .fields
            .contains(&"cleanup_deadline_ms".to_string()));
        assert_eq!(
            contract.managed_services.fields,
            string_vec(AgentTaskManagedService::FIELDS)
        );
        assert_eq!(
            contract.managed_services.lifecycle_values,
            string_vec(crate::agent_task_schedule::AgentTaskManagedServiceLifecycle::VALUES)
        );
        assert_eq!(
            contract.managed_services.default_cleanup_deadline_ms,
            AgentTaskManagedService::DEFAULT_CLEANUP_DEADLINE_MS
        );
        assert_eq!(
            contract.managed_services.max_cleanup_deadline_ms,
            AgentTaskManagedService::MAX_CLEANUP_DEADLINE_MS
        );
    }
}
