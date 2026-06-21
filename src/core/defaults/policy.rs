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
/// locally (via `--force-hot --allow-local-hot` or a stale-runner fallback)
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

pub(crate) fn resolve_release_gate_local_hot_policy_from(
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
