//! Transport-neutral runner capability and readiness requests.

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

/// Extension-owned command that proves a runner toolchain is usable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunnerToolchainReadinessProbe {
    pub extension_id: String,
    pub id: String,
    pub program: String,
    pub args: Vec<String>,
    pub repair_command: Option<String>,
    pub diagnostic_env: Vec<String>,
}

/// A resolved set of capability requirements to preflight before running a
/// command on a runner.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunnerCapabilityPreflight {
    pub command: String,
    pub required_tools: Vec<RunnerRequiredTool>,
    pub required_commands: Vec<String>,
    pub required_tool_capabilities: Vec<RunnerToolCapabilityRequirement>,
    pub required_toolchain_probes: Vec<RunnerToolchainReadinessProbe>,
    pub required_components: Vec<String>,
    pub required_env: Vec<String>,
    pub timeout: Option<std::time::Duration>,
}

impl RunnerCapabilityPreflight {
    pub fn is_empty(&self) -> bool {
        self.required_tools.is_empty()
            && self.required_commands.is_empty()
            && self.required_tool_capabilities.is_empty()
            && self.required_toolchain_probes.is_empty()
            && self.required_components.is_empty()
            && self.required_env.is_empty()
    }
}
