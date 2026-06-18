//! Generic command execution and Lab route plan contracts.

use serde::Serialize;

use crate::command_contract::LabRoutingPolicy;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CommandExecutionPlan {
    pub label: String,
    pub portability: CommandPortability,
    pub remote_argv: Vec<String>,
    pub source_policy: CommandSourcePolicy,
    pub workspace_policy: CommandWorkspacePolicy,
    pub output_contract: CommandOutputContract,
}

impl CommandExecutionPlan {
    pub fn remote(
        label: impl Into<String>,
        remote_argv: Vec<String>,
        source_policy: CommandSourcePolicy,
        workspace_policy: CommandWorkspacePolicy,
        output_contract: CommandOutputContract,
    ) -> Self {
        Self {
            label: label.into(),
            portability: CommandPortability::Portable,
            remote_argv,
            source_policy,
            workspace_policy,
            output_contract,
        }
    }

    pub fn local_only(label: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            portability: CommandPortability::LocalOnly {
                reason: reason.into(),
            },
            remote_argv: Vec::new(),
            source_policy: CommandSourcePolicy::ControllerCwdOrExplicitPath,
            workspace_policy: CommandWorkspacePolicy::Snapshot,
            output_contract: CommandOutputContract::inherit(),
        }
    }

    pub fn safe_remote_argv(&self) -> Option<&[String]> {
        matches!(self.portability, CommandPortability::Portable).then_some(&self.remote_argv)
    }

    pub fn local_only_reason(&self) -> Option<&str> {
        match &self.portability {
            CommandPortability::Portable => None,
            CommandPortability::LocalOnly { reason } => Some(reason),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum CommandPortability {
    Portable,
    LocalOnly { reason: String },
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommandSourcePolicy {
    ControllerCwdOrExplicitPath,
    RunnerResident,
    MaterializeControllerPath,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommandWorkspacePolicy {
    ChangedSinceGitElseSnapshot,
    Git,
    GitCheckoutRequired,
    RunnerResident,
    Snapshot,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CommandOutputContract {
    pub format: CommandOutputFormat,
    pub includes_execution: bool,
    pub includes_plan: bool,
}

impl CommandOutputContract {
    pub const fn inherit() -> Self {
        Self {
            format: CommandOutputFormat::Inherit,
            includes_execution: false,
            includes_plan: false,
        }
    }

    pub const fn structured_json_with_execution_plan() -> Self {
        Self {
            format: CommandOutputFormat::StructuredJson,
            includes_execution: true,
            includes_plan: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommandOutputFormat {
    Inherit,
    StructuredJson,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LabRoutePlan {
    pub label: String,
    pub portability: CommandPortability,
    pub source_policy: CommandSourcePolicy,
    pub workspace_policy: CommandWorkspacePolicy,
    pub output_contract: CommandOutputContract,
    pub required_extensions: Vec<String>,
    pub requires_playwright: bool,
    /// Routing-policy flags shared across the Lab command layers. Flattened so
    /// the serialized shape keeps `default_lab_offload`, `infer_source_path_tools`,
    /// `release_gate`, and `requires_extension_parity` as top-level keys.
    #[serde(flatten)]
    pub routing_policy: LabRoutingPolicy,
}

impl LabRoutePlan {
    pub fn portable(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            portability: CommandPortability::Portable,
            source_policy: CommandSourcePolicy::ControllerCwdOrExplicitPath,
            workspace_policy: CommandWorkspacePolicy::ChangedSinceGitElseSnapshot,
            output_contract: CommandOutputContract::inherit(),
            required_extensions: Vec::new(),
            requires_playwright: false,
            routing_policy: LabRoutingPolicy::default(),
        }
    }

    pub fn local_only(label: impl Into<String>, reason: impl Into<String>) -> Self {
        let mut plan = Self::portable(label);
        plan.portability = CommandPortability::LocalOnly {
            reason: reason.into(),
        };
        plan
    }

    pub fn local_only_reason(&self) -> Option<&str> {
        match &self.portability {
            CommandPortability::Portable => None,
            CommandPortability::LocalOnly { reason } => Some(reason),
        }
    }
}
