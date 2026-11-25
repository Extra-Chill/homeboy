use serde::{Deserialize, Serialize};

use super::{load_config, HomeboyConfig};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BenchConfig {
    #[serde(default)]
    pub local_execution: BenchLocalExecutionPolicy,
}

impl Default for BenchConfig {
    fn default() -> Self {
        Self {
            local_execution: BenchLocalExecutionPolicy::Allowed,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum BenchLocalExecutionPolicy {
    #[default]
    Allowed,
    Denied,
}

impl BenchLocalExecutionPolicy {
    pub fn is_denied(self) -> bool {
        matches!(self, Self::Denied)
    }
}

/// Release-gate routing safety policy.
///
/// Release gates are the quality-check hot commands (lint/test/audit) whose
/// routing fidelity matters for validating a release. When a default Lab
/// runner is configured, silently bypassing Lab routing to run these gates
/// locally (through an explicit placement request or a stale-runner fallback)
/// produces a gate result that is not faithful to the configured runner
/// policy. This config makes such bypasses fail closed with a clear
/// diagnostic instead of silently executing locally. See issues #4603 / #4605.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ReleaseGateConfig {
    /// Whether release-gate hot commands may be bypassed to local execution
    /// when a default Lab runner is configured.
    ///
    /// - `fail_closed` (default): the bypass is rejected with a diagnostic.
    /// - `allowed`: the bypass runs locally and is recorded in the offload
    ///   metadata (the operator-only override).
    #[serde(default)]
    pub local_hot: ReleaseGateLocalHotPolicy,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ReleaseGateLocalHotPolicy {
    /// Reject force-local bypass and stale-runner local fallback for release
    /// gates when a default Lab runner is configured.
    #[default]
    FailClosed,
    /// Allow release gates to run locally; recorded in offload metadata.
    Allowed,
}

impl ReleaseGateLocalHotPolicy {
    pub fn is_allowed(self) -> bool {
        matches!(self, Self::Allowed)
    }
}

/// Environment variable override for `/release_gate/local_hot`.
///
/// Takes precedence over the config file so operators can re-enable a
/// local-hot bypass for a single invocation without editing config. This is
/// the explicit operator-only override: it must be set in the environment, not
/// via a convenience CLI flag, so it cannot become a habit bypass.
pub const RELEASE_GATE_LOCAL_HOT_ENV: &str = "HOMEBOY_RELEASE_GATE_LOCAL_HOT";

/// Resolve the effective release-gate local-hot policy from the environment
/// override (precedence) then the config file, falling back to the default.
pub fn resolve_release_gate_local_hot_policy() -> ReleaseGateLocalHotPolicy {
    resolve_release_gate_local_hot_policy_from(&load_config())
}

pub fn resolve_release_gate_local_hot_policy_from(
    config: &HomeboyConfig,
) -> ReleaseGateLocalHotPolicy {
    if let Ok(raw) = std::env::var(RELEASE_GATE_LOCAL_HOT_ENV) {
        match raw.trim().to_ascii_lowercase().as_str() {
            "allowed" | "allow" | "true" | "1" => return ReleaseGateLocalHotPolicy::Allowed,
            "fail_closed" | "fail-closed" | "denied" | "false" | "0" => {
                return ReleaseGateLocalHotPolicy::FailClosed;
            }
            _ => {}
        }
    }
    config.release_gate.local_hot
}

/// Agent-task local (controller) execution policy, `/agent_task/local_execution`.
///
/// `denied` forbids every agent-task submission whose placement decision
/// selects local execution on this host — explicit `--placement local`,
/// `lab-or-local` falling back, and `auto` falling back to local — so work
/// stays on the Lab. Runner-resident execution (the provider attempt a Lab
/// runner runs on behalf of the controller) is not controller execution and
/// is unaffected. Defaults to `allowed` so existing installs are unchanged.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskLocalExecutionPolicy {
    #[default]
    Allowed,
    Denied,
}

impl AgentTaskLocalExecutionPolicy {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allowed)
    }

    pub fn is_denied(self) -> bool {
        matches!(self, Self::Denied)
    }
}

/// Config pointer that owns [`AgentTaskLocalExecutionPolicy`].
pub const AGENT_TASK_LOCAL_EXECUTION_POINTER: &str = "/agent_task/local_execution";

/// Resolve the effective agent-task local-execution policy from config.
pub fn resolve_agent_task_local_execution_policy() -> AgentTaskLocalExecutionPolicy {
    load_config().agent_task.local_execution
}

/// The admission error for an agent-task submission whose placement selected
/// local execution while `/agent_task/local_execution` is `denied`.
///
/// `reason` carries the routing reason (for example the Lab runner readiness
/// reason behind an `auto` fallback) so the operator sees why Lab was not
/// selected instead of a generic refusal.
pub fn agent_task_local_execution_denied_error(reason: Option<&str>) -> crate::Error {
    let reason = reason.map(str::trim).filter(|reason| !reason.is_empty());
    let message = match reason {
        Some(reason) => format!(
            "local agent-task execution is disabled by `{AGENT_TASK_LOCAL_EXECUTION_POINTER}` (denied); placement selected local execution: {reason}"
        ),
        None => format!(
            "local agent-task execution is disabled by `{AGENT_TASK_LOCAL_EXECUTION_POINTER}` (denied); placement selected local execution"
        ),
    };
    let mut hints = vec![
        "Use `--placement lab` or `--placement auto` so the attempt runs on a Lab runner."
            .to_string(),
        "Check Lab readiness with `homeboy runner status <id>`.".to_string(),
        format!(
            "To allow controller execution again: `homeboy config set {AGENT_TASK_LOCAL_EXECUTION_POINTER} allowed`."
        ),
    ];
    if let Some(reason) = reason {
        hints.insert(0, reason.to_string());
    }
    crate::Error::validation_invalid_argument(
        "agent_task.local_execution",
        message,
        Some("denied".to_string()),
        Some(hints),
    )
}
