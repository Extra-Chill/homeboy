use super::AgentToolExecutionLocation;

pub const AGENT_TASK_REQUEST_SCHEMA: &str = "homeboy/agent-task-request/v1";
pub const AGENT_TASK_OUTCOME_SCHEMA: &str = "homeboy/agent-task-outcome/v1";
pub const AGENT_TASK_ARTIFACT_SCHEMA: &str = "homeboy/agent-task-artifact/v1";
pub const AGENT_TASK_WORKFLOW_SCHEMA: &str = "homeboy/agent-task-workflow/v1";
pub const AGENT_TASK_MATRIX_PLAN_SCHEMA: &str = "homeboy/agent-task-matrix-plan/v1";
pub const AGENT_TASK_MATRIX_AGGREGATE_SCHEMA: &str = "homeboy/agent-task-matrix-aggregate/v1";
pub const AGENT_TOOL_REQUEST_SCHEMA: &str = "homeboy/agent-tool-request/v1";
pub const AGENT_TOOL_RESULT_SCHEMA: &str = "homeboy/agent-tool-result/v1";
pub const AGENT_TOOL_POLICY_SCHEMA: &str = "homeboy/agent-tool-policy/v1";

pub(crate) fn request_schema() -> String {
    AGENT_TASK_REQUEST_SCHEMA.to_string()
}

pub(crate) fn outcome_schema() -> String {
    AGENT_TASK_OUTCOME_SCHEMA.to_string()
}

pub(crate) fn artifact_schema() -> String {
    AGENT_TASK_ARTIFACT_SCHEMA.to_string()
}

pub(crate) fn workflow_schema() -> String {
    AGENT_TASK_WORKFLOW_SCHEMA.to_string()
}

pub(crate) fn agent_tool_request_schema() -> String {
    AGENT_TOOL_REQUEST_SCHEMA.to_string()
}

pub(crate) fn agent_tool_result_schema() -> String {
    AGENT_TOOL_RESULT_SCHEMA.to_string()
}

pub(crate) fn agent_tool_policy_schema() -> String {
    AGENT_TOOL_POLICY_SCHEMA.to_string()
}

pub(crate) fn default_agent_tool_execution_location() -> AgentToolExecutionLocation {
    AgentToolExecutionLocation::Disabled
}

pub(crate) fn default_read_policy() -> String {
    "workspace".to_string()
}

pub(crate) fn default_write_policy() -> String {
    "artifacts_only".to_string()
}

pub(crate) fn default_apply_policy() -> String {
    "propose_only".to_string()
}
