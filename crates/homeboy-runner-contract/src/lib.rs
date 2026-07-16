//! Shared runner contract: behavior-free types and env-var constants that both
//! `homeboy-core` and the optional `homeboy-runner` feature crate depend on.
//!
//! Runner is an optional Lab-offload feature; core must not depend on runner
//! *behavior*. But some core code legitimately needs to name runner *concepts*
//! (e.g. the runner kind, or the env-var markers used when an exec crosses a
//! remote-runner boundary). Those plain-data contracts live here so core can
//! reference them without a `core -> runner` edge.

use serde::{Deserialize, Serialize};

/// The kind of runner backing a homeboy runner definition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerKind {
    Local,
    Ssh,
}

/// Set while a hosted exec runs inside a runner (as opposed to the local host).
pub const RUNNER_HOSTED_EXEC_ENV: &str = "HOMEBOY_RUNNER_HOSTED_EXEC";

/// Private process marker added only while a runner exec crosses a remote
/// runner boundary. Intentionally absent from CLI parsing and argv.
pub const RUNNER_PLACEMENT_RESOLVED_ENV: &str = "HOMEBOY_RUNNER_PLACEMENT_RESOLVED";

/// Identifies the runner an exec is bound to.
pub const RUNNER_ID_ENV: &str = "HOMEBOY_RUNNER_ID";

/// A tool that must be present on a runner for a capability to be satisfied.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RunnerRequiredTool {
    id: String,
}

impl RunnerRequiredTool {
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into() }
    }

    pub fn homeboy() -> Self {
        Self::new("homeboy")
    }

    pub fn git() -> Self {
        Self::new("git")
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}

/// A tool + command capability requirement probed on a runner.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunnerToolCapabilityRequirement {
    pub tool: String,
    pub command: String,
    pub env: Vec<String>,
    pub capabilities: Vec<String>,
}

/// A resolved set of capability requirements to preflight before running a
/// command on a runner.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunnerCapabilityPreflight {
    pub command: String,
    pub required_tools: Vec<RunnerRequiredTool>,
    pub required_commands: Vec<String>,
    pub required_tool_capabilities: Vec<RunnerToolCapabilityRequirement>,
    pub required_components: Vec<String>,
    pub required_env: Vec<String>,
    pub timeout: Option<std::time::Duration>,
}

impl RunnerCapabilityPreflight {
    pub fn is_empty(&self) -> bool {
        self.required_tools.is_empty()
            && self.required_commands.is_empty()
            && self.required_tool_capabilities.is_empty()
            && self.required_components.is_empty()
            && self.required_env.is_empty()
    }
}

/// A lab runner capability prepared from a contract, ready to preflight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedLabRunnerCapability {
    pub command: &'static str,
    pub required_tools: Vec<RunnerRequiredTool>,
}

impl From<PreparedLabRunnerCapability> for RunnerCapabilityPreflight {
    fn from(plan: PreparedLabRunnerCapability) -> Self {
        Self {
            command: plan.command.to_string(),
            required_tools: plan.required_tools,
            required_commands: Vec::new(),
            required_tool_capabilities: Vec::new(),
            required_components: Vec::new(),
            required_env: Vec::new(),
            timeout: None,
        }
    }
}

/// The capability contract a lab runner must satisfy for a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabRunnerCapabilityContract {
    pub command: &'static str,
    pub required_tools: Vec<RunnerRequiredTool>,
    pub required_capabilities: Vec<String>,
}

/// How a lab runner capability gate is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabRunnerGateMode {
    Automatic,
    Explicit,
}

/// The outcome of evaluating a lab runner capability gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabRunnerGateDecision {
    Eligible,
    Missing {
        runner_id: String,
        command: &'static str,
        missing_tools: Vec<RunnerRequiredTool>,
        reason: String,
        remediation: Vec<String>,
    },
}
