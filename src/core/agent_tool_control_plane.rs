use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::core::agent_task::{
    AgentTaskDiagnostic, AgentToolExecutionLocation, AgentToolPolicy, AgentToolRequest,
    AgentToolResult, AgentToolResultStatus, AGENT_TOOL_RESULT_SCHEMA,
};

pub const AGENT_TOOL_DISPATCH_EVIDENCE_SCHEMA: &str = "homeboy/agent-tool-dispatch-evidence/v1";

pub trait AgentToolControlPlaneDispatcher {
    fn dispatch(&self, request: &AgentToolRequest) -> AgentToolResult;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct UnsupportedAgentToolControlPlaneDispatcher;

impl AgentToolControlPlaneDispatcher for UnsupportedAgentToolControlPlaneDispatcher {
    fn dispatch(&self, request: &AgentToolRequest) -> AgentToolResult {
        unsupported_control_plane_result(request)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentToolDispatchOutcome {
    pub location: AgentToolExecutionLocation,
    pub result: AgentToolResult,
    pub evidence: AgentToolDispatchEvidence,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentToolDispatchEvidence {
    pub schema: String,
    pub location: AgentToolExecutionLocation,
    pub request: AgentToolRequest,
    pub result: AgentToolResult,
}

pub fn dispatch_agent_tool_request(
    policy: &AgentToolPolicy,
    request: &AgentToolRequest,
    dispatcher: &impl AgentToolControlPlaneDispatcher,
) -> AgentToolDispatchOutcome {
    let location = policy.execution_location_for(&request.tool);
    let result = match location {
        AgentToolExecutionLocation::Disabled => disabled_tool_result(request),
        AgentToolExecutionLocation::ControlPlane => dispatcher.dispatch(request),
        AgentToolExecutionLocation::Runner => runner_owned_tool_result(request),
    };
    let evidence = AgentToolDispatchEvidence {
        schema: AGENT_TOOL_DISPATCH_EVIDENCE_SCHEMA.to_string(),
        location,
        request: request.redacted(),
        result: result.redacted(),
    };

    AgentToolDispatchOutcome {
        location,
        result,
        evidence,
    }
}

fn disabled_tool_result(request: &AgentToolRequest) -> AgentToolResult {
    AgentToolResult {
        schema: AGENT_TOOL_RESULT_SCHEMA.to_string(),
        request_id: request.request_id.clone(),
        task_id: request.task_id.clone(),
        tool: request.tool.clone(),
        status: AgentToolResultStatus::Denied,
        output: Value::Null,
        diagnostics: vec![AgentTaskDiagnostic {
            class: "agent_tool.disabled".to_string(),
            message: format!("tool '{}' is disabled by agent tool policy", request.tool),
            data: json!({ "tool": request.tool }),
        }],
        metadata: json!({ "execution_location": "disabled" }),
    }
}

fn runner_owned_tool_result(request: &AgentToolRequest) -> AgentToolResult {
    AgentToolResult {
        schema: AGENT_TOOL_RESULT_SCHEMA.to_string(),
        request_id: request.request_id.clone(),
        task_id: request.task_id.clone(),
        tool: request.tool.clone(),
        status: AgentToolResultStatus::Failed,
        output: Value::Null,
        diagnostics: vec![AgentTaskDiagnostic {
            class: "agent_tool.runner_dispatch_not_handled".to_string(),
            message: "runner tool execution is owned by the provider runtime, not the control-plane dispatcher".to_string(),
            data: json!({ "tool": request.tool }),
        }],
        metadata: json!({ "execution_location": "runner" }),
    }
}

fn unsupported_control_plane_result(request: &AgentToolRequest) -> AgentToolResult {
    AgentToolResult {
        schema: AGENT_TOOL_RESULT_SCHEMA.to_string(),
        request_id: request.request_id.clone(),
        task_id: request.task_id.clone(),
        tool: request.tool.clone(),
        status: AgentToolResultStatus::Failed,
        output: Value::Null,
        diagnostics: vec![AgentTaskDiagnostic {
            class: "agent_tool.control_plane_dispatch_unsupported".to_string(),
            message: "control-plane tool dispatch is selected by policy, but no dispatcher is registered for this provider execution".to_string(),
            data: json!({ "tool": request.tool }),
        }],
        metadata: json!({ "execution_location": "control_plane" }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use crate::core::agent_task::{
        AgentToolPolicyRule, AGENT_TOOL_POLICY_SCHEMA, AGENT_TOOL_REQUEST_SCHEMA,
    };

    #[derive(Debug, Clone, Copy)]
    struct EchoDispatcher;

    impl AgentToolControlPlaneDispatcher for EchoDispatcher {
        fn dispatch(&self, request: &AgentToolRequest) -> AgentToolResult {
            AgentToolResult {
                schema: AGENT_TOOL_RESULT_SCHEMA.to_string(),
                request_id: request.request_id.clone(),
                task_id: request.task_id.clone(),
                tool: request.tool.clone(),
                status: AgentToolResultStatus::Succeeded,
                output: json!({ "token": "secret-output", "safe": true }),
                diagnostics: Vec::new(),
                metadata: json!({ "authorization": "Bearer result-secret" }),
            }
        }
    }

    fn request(tool: &str) -> AgentToolRequest {
        AgentToolRequest {
            schema: AGENT_TOOL_REQUEST_SCHEMA.to_string(),
            request_id: "request-1".to_string(),
            task_id: "task-1".to_string(),
            tool: tool.to_string(),
            input: json!({ "token": "secret-input", "safe": true }),
            timeout_ms: None,
            metadata: json!({ "password": "secret-metadata" }),
        }
    }

    fn policy(default_location: AgentToolExecutionLocation) -> AgentToolPolicy {
        AgentToolPolicy {
            schema: AGENT_TOOL_POLICY_SCHEMA.to_string(),
            default_location,
            tools: BTreeMap::new(),
        }
    }

    #[test]
    fn tool_policy_selects_explicit_route_over_default() {
        let mut policy = policy(AgentToolExecutionLocation::Disabled);
        policy.tools.insert(
            "lookup".to_string(),
            AgentToolPolicyRule {
                execution_location: AgentToolExecutionLocation::ControlPlane,
                timeout_ms: Some(250),
                reason: Some("test route".to_string()),
            },
        );

        let outcome = dispatch_agent_tool_request(&policy, &request("lookup"), &EchoDispatcher);

        assert_eq!(outcome.location, AgentToolExecutionLocation::ControlPlane);
        assert_eq!(outcome.result.status, AgentToolResultStatus::Succeeded);
    }

    #[test]
    fn tool_policy_is_disabled_by_default() {
        let outcome = dispatch_agent_tool_request(
            &AgentToolPolicy::default(),
            &request("lookup"),
            &EchoDispatcher,
        );

        assert_eq!(outcome.location, AgentToolExecutionLocation::Disabled);
        assert_eq!(outcome.result.status, AgentToolResultStatus::Denied);
        assert_eq!(outcome.result.diagnostics[0].class, "agent_tool.disabled");
    }

    #[test]
    fn tool_dispatch_evidence_redacts_request_and_result() {
        let outcome = dispatch_agent_tool_request(
            &policy(AgentToolExecutionLocation::ControlPlane),
            &request("lookup"),
            &EchoDispatcher,
        );

        assert_eq!(outcome.evidence.schema, AGENT_TOOL_DISPATCH_EVIDENCE_SCHEMA);
        assert_eq!(outcome.evidence.request.input["token"], "[REDACTED]");
        assert_eq!(outcome.evidence.request.input["safe"], true);
        assert_eq!(outcome.evidence.request.metadata["password"], "[REDACTED]");
        assert_eq!(outcome.evidence.result.output["token"], "[REDACTED]");
        assert_eq!(
            outcome.evidence.result.metadata["authorization"],
            "[REDACTED]"
        );
    }

    #[test]
    fn unsupported_control_plane_dispatch_returns_explicit_diagnostic() {
        let outcome = dispatch_agent_tool_request(
            &policy(AgentToolExecutionLocation::ControlPlane),
            &request("lookup"),
            &UnsupportedAgentToolControlPlaneDispatcher,
        );

        assert_eq!(outcome.location, AgentToolExecutionLocation::ControlPlane);
        assert_eq!(outcome.result.status, AgentToolResultStatus::Failed);
        assert_eq!(
            outcome.result.diagnostics[0].class,
            "agent_tool.control_plane_dispatch_unsupported"
        );
        assert_eq!(
            outcome.evidence.result.diagnostics[0].class,
            "agent_tool.control_plane_dispatch_unsupported"
        );
    }
}
