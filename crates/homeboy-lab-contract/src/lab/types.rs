//! Lab portability contract types and construction policy.

use super::workload::RunnerWorkloadCapability;

pub const LAB_CAPABILITY_PLAYWRIGHT: &str = "playwright";
pub const LAB_TRACE_EXTRA_CAPABILITIES: &[&str] = &[LAB_CAPABILITY_PLAYWRIGHT];
pub const LAB_NO_SECRET_ENV_SOURCES: &[LabSecretEnvSource] = &[];
pub const LAB_NO_EXTRA_CAPABILITIES: &[&str] = &[];
pub const LAB_AGENT_TASK_SECRET_ENV_SOURCES: &[LabSecretEnvSource] =
    &[LabSecretEnvSource::AgentTask];
pub const LAB_TRACE_SECRET_ENV_SOURCES: &[LabSecretEnvSource] = &[LabSecretEnvSource::Trace];
pub const LAB_TUNNEL_SECRET_ENV_SOURCES: &[LabSecretEnvSource] = &[LabSecretEnvSource::Tunnel];
pub const RIG_UP_LAB_UNSUPPORTED_REASON: &str = "`rig up` stays local because rig pipelines manage local services, leases, ports, and declared filesystem paths that the current single-workspace Lab snapshot cannot safely mirror. For Lab/offloaded dependency preparation and verification, run `homeboy rig check <rig-id> --runner <runner-id>` or the rig's benchmark profile through `homeboy rig run <rig-id> --runner <runner-id>`.";
pub const RIG_SOURCE_MANAGEMENT_LAB_UNSUPPORTED_REASON: &str = "rig source-management commands (`rig install`, `rig update`, `rig sync`, and `rig sources`) manage the controller's local rig registry and may read arbitrary local package paths. They are not Lab-portable yet. Install or refresh the rig locally first, then run Lab-compatible verification with `homeboy rig check <rig-id> --runner <runner-id>` or `homeboy rig run <rig-id> --runner <runner-id>`.";

/// Routing-policy flags owned by the Lab command contract and retained through
/// route planning, offload, and runner dispatch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LabRoutingPolicy {
    /// Whether the command offloads to a default Lab runner without an explicit
    /// `--runner` selection.
    pub default_lab_offload: bool,
    /// Whether source-path tool inference applies to this command.
    pub infer_source_path_tools: bool,
    /// Whether this command is a release gate whose routing fidelity matters
    /// for validating a release (lint/test/audit). When true, force-local
    /// bypass and stale-runner local fallback fail closed under the
    /// `/release_gate/local_hot` policy if a default Lab runner is configured.
    /// See issues #4603 / #4605.
    pub release_gate: bool,
    /// Whether the command requires extension parity between controller and
    /// runner before offloading.
    pub requires_extension_parity: bool,
    /// Whether runner-resident reads should avoid durable mirror runs and
    /// handoff boilerplate unless the command itself emits them.
    pub read_only_polling: bool,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct LabCommandRouteContract {
    pub command: LabCommandContract,
    pub required_extensions: Vec<String>,
    pub required_capabilities: Vec<RunnerWorkloadCapability>,
    pub workload: Option<LabRigWorkloadArguments>,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct LabRigWorkloadArguments {
    pub kind: LabRigWorkloadKind,
    pub rig_ids: Vec<String>,
    pub component: Option<String>,
    pub extension_overrides: Vec<String>,
}

#[derive(Debug, Clone, Copy, serde::Serialize, PartialEq, Eq)]
pub enum LabRigWorkloadKind {
    Bench,
    Fuzz,
}

#[derive(Debug, Clone, Copy, serde::Serialize, PartialEq, Eq)]
pub enum LabSecretEnvSource {
    AgentTask,
    Trace,
    Tunnel,
}

#[derive(Debug, Clone, Copy, serde::Serialize, PartialEq, Eq)]
pub struct LabCommandContract {
    pub hot_label: &'static str,
    pub portability: LabCommandPortability,
    pub source_path_mode: LabSourcePathMode,
    pub workspace_mode_policy: LabWorkspaceModePolicy,
    pub capture_mutation_patch: bool,
    pub mutation_flag: Option<&'static str>,
    pub extra_required_capabilities: &'static [&'static str],
    pub secret_env_sources: &'static [LabSecretEnvSource],
    /// Routing-policy flags shared across the Lab command layers.
    pub routing_policy: LabRoutingPolicy,
}

#[derive(Debug, Clone, Copy, Default, serde::Serialize, PartialEq, Eq)]
pub struct CommandPortabilityContract {
    lab_command: Option<LabCommandContract>,
}

impl CommandPortabilityContract {
    pub const fn none() -> Self {
        Self { lab_command: None }
    }

    pub const fn lab(command: LabCommandContract) -> Self {
        Self {
            lab_command: Some(command),
        }
    }

    pub const fn lab_optional(command: Option<LabCommandContract>) -> Self {
        Self {
            lab_command: command,
        }
    }

    pub const fn lab_command(self) -> Option<LabCommandContract> {
        self.lab_command
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize, PartialEq, Eq)]
pub enum LabCommandPortability {
    Portable,
    LocalOnly(&'static str),
}

#[derive(Debug, Clone, Copy, serde::Serialize, PartialEq, Eq)]
pub enum LabSourcePathMode {
    CwdOrPathFlag,
    RunnerResident,
}

#[derive(Debug, Clone, Copy, serde::Serialize, PartialEq, Eq)]
pub enum LabWorkspaceModePolicy {
    ChangedSinceGitElseSnapshot,
    Git,
    GitCheckoutRequired,
    RunnerResident,
}

impl LabCommandContract {
    pub const fn is_portable(self) -> bool {
        matches!(self.portability, LabCommandPortability::Portable)
    }

    pub fn into_route_contract(self, required_extensions: Vec<String>) -> LabCommandRouteContract {
        let required_capabilities = self
            .extra_required_capabilities
            .iter()
            .map(|capability| RunnerWorkloadCapability {
                name: (*capability).to_string(),
                required: true,
            })
            .collect();
        LabCommandRouteContract {
            command: self,
            required_extensions,
            required_capabilities,
            workload: None,
        }
    }

    pub fn portable(
        hot_label: &'static str,
        mutation_flag: Option<&'static str>,
        requires_extension_parity: bool,
        extra_required_capabilities: &'static [&'static str],
    ) -> Self {
        Self {
            hot_label,
            portability: LabCommandPortability::Portable,
            source_path_mode: LabSourcePathMode::CwdOrPathFlag,
            workspace_mode_policy: LabWorkspaceModePolicy::ChangedSinceGitElseSnapshot,
            capture_mutation_patch: mutation_flag.is_some(),
            mutation_flag,
            extra_required_capabilities,
            secret_env_sources: LAB_NO_SECRET_ENV_SOURCES,
            routing_policy: LabRoutingPolicy {
                default_lab_offload: true,
                infer_source_path_tools: true,
                release_gate: false,
                requires_extension_parity,
                read_only_polling: false,
            },
        }
    }

    pub fn portable_workload(
        hot_label: &'static str,
        mutation_flag: Option<&'static str>,
        requires_extension_parity: bool,
        extra_required_capabilities: &'static [&'static str],
    ) -> Self {
        let base = Self::portable(
            hot_label,
            mutation_flag,
            requires_extension_parity,
            extra_required_capabilities,
        );
        Self {
            routing_policy: LabRoutingPolicy {
                infer_source_path_tools: false,
                ..base.routing_policy
            },
            ..base
        }
    }

    pub fn explicit_runner(
        hot_label: &'static str,
        mutation_flag: Option<&'static str>,
        requires_extension_parity: bool,
        extra_required_capabilities: &'static [&'static str],
    ) -> Self {
        let base = Self::portable_workload(
            hot_label,
            mutation_flag,
            requires_extension_parity,
            extra_required_capabilities,
        );
        Self {
            routing_policy: LabRoutingPolicy {
                default_lab_offload: false,
                ..base.routing_policy
            },
            ..base
        }
    }

    pub fn explicit_runner_simple(hot_label: &'static str) -> Self {
        Self::explicit_runner(hot_label, None, false, LAB_NO_EXTRA_CAPABILITIES)
    }

    pub fn runner_resident(hot_label: &'static str) -> Self {
        Self {
            source_path_mode: LabSourcePathMode::RunnerResident,
            workspace_mode_policy: LabWorkspaceModePolicy::RunnerResident,
            ..Self::explicit_runner_simple(hot_label)
        }
    }

    pub fn runner_resident_read_polling(hot_label: &'static str) -> Self {
        let base = Self::runner_resident(hot_label);
        Self {
            routing_policy: LabRoutingPolicy {
                read_only_polling: true,
                ..base.routing_policy
            },
            ..base
        }
    }

    pub fn local_only(hot_label: &'static str, reason: &'static str) -> Self {
        Self {
            hot_label,
            portability: LabCommandPortability::LocalOnly(reason),
            source_path_mode: LabSourcePathMode::CwdOrPathFlag,
            workspace_mode_policy: LabWorkspaceModePolicy::ChangedSinceGitElseSnapshot,
            capture_mutation_patch: false,
            mutation_flag: None,
            extra_required_capabilities: LAB_NO_EXTRA_CAPABILITIES,
            secret_env_sources: LAB_NO_SECRET_ENV_SOURCES,
            routing_policy: LabRoutingPolicy {
                default_lab_offload: false,
                infer_source_path_tools: false,
                release_gate: false,
                requires_extension_parity: false,
                read_only_polling: false,
            },
        }
    }

    pub const fn release_gate(mut self) -> Self {
        self.routing_policy.release_gate = true;
        self
    }

    pub const fn with_hot_label(mut self, hot_label: &'static str) -> Self {
        self.hot_label = hot_label;
        self
    }

    pub const fn with_secret_env_sources(mut self, sources: &'static [LabSecretEnvSource]) -> Self {
        self.secret_env_sources = sources;
        self
    }
}
