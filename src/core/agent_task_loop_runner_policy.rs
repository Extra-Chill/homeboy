use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::agent_task_loop_controller::{
    AgentTaskLoopActionDiagnostic, AgentTaskLoopActionStatus, AgentTaskLoopPolicyAction,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskLoopRunnerPolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_fallback: Option<AgentTaskLoopLocalFallbackPolicy>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskLoopLocalFallbackPolicy {
    Allowed,
    Denied,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentTaskLoopRunnerAvailability {
    Available,
    Unavailable { reason: String },
    MaterializationBlocked { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentTaskLoopRunnerExecutionTarget {
    Local,
    Runner(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTaskLoopRunnerPolicyDecision {
    pub target: Option<AgentTaskLoopRunnerExecutionTarget>,
    pub blocked_status: Option<AgentTaskLoopActionStatus>,
    pub diagnostic: Option<AgentTaskLoopActionDiagnostic>,
}

pub(crate) fn runner_policy_for_action(
    action: &AgentTaskLoopPolicyAction,
) -> AgentTaskLoopRunnerPolicy {
    let request = action_runner_request(action);
    let runner = request
        .and_then(|value| value.get("runner"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|runner| !runner.is_empty())
        .map(ToString::to_string);
    let local_fallback = request
        .and_then(|value| value.get("local_fallback"))
        .and_then(parse_local_fallback_policy);

    AgentTaskLoopRunnerPolicy {
        runner,
        local_fallback,
    }
}

pub(crate) fn blocked_runner_decision(
    status: AgentTaskLoopActionStatus,
    runner: Option<String>,
    message: impl Into<String>,
    details: Value,
) -> AgentTaskLoopRunnerPolicyDecision {
    AgentTaskLoopRunnerPolicyDecision {
        target: None,
        blocked_status: Some(status),
        diagnostic: Some(AgentTaskLoopActionDiagnostic {
            code: blocked_runner_status_code(status).to_string(),
            message: message.into(),
            runner,
            details,
        }),
    }
}

fn action_runner_request(action: &AgentTaskLoopPolicyAction) -> Option<&Value> {
    match action {
        AgentTaskLoopPolicyAction::SpawnTask { request, .. } => Some(request),
        AgentTaskLoopPolicyAction::FanOut {
            request_template, ..
        }
        | AgentTaskLoopPolicyAction::RouteFinding {
            request_template, ..
        } => Some(request_template),
        _ => None,
    }
}

fn parse_local_fallback_policy(value: &Value) -> Option<AgentTaskLoopLocalFallbackPolicy> {
    match value {
        Value::Bool(true) => Some(AgentTaskLoopLocalFallbackPolicy::Allowed),
        Value::Bool(false) => Some(AgentTaskLoopLocalFallbackPolicy::Denied),
        Value::String(value) if value == "allowed" || value == "allow" || value == "true" => {
            Some(AgentTaskLoopLocalFallbackPolicy::Allowed)
        }
        Value::String(value) if value == "denied" || value == "deny" || value == "false" => {
            Some(AgentTaskLoopLocalFallbackPolicy::Denied)
        }
        _ => None,
    }
}

fn blocked_runner_status_code(status: AgentTaskLoopActionStatus) -> &'static str {
    match status {
        AgentTaskLoopActionStatus::BlockedRunnerUnavailable => "blocked_runner_unavailable",
        AgentTaskLoopActionStatus::BlockedRemoteMaterialization => "blocked_remote_materialization",
        AgentTaskLoopActionStatus::BlockedLocalFallbackDenied => "blocked_local_fallback_denied",
        _ => "runner_policy_not_blocked",
    }
}
