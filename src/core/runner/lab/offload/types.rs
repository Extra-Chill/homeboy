//! Public request/command/outcome types for Lab offload.

use std::collections::HashMap;

use crate::core::plan::HomeboyPlan;
use crate::core::runner_execution_envelope::PathMaterializationPlan;
use crate::core::source_snapshot::SourceSnapshot;

pub use crate::command_contract::LabLocalExecutionPolicy;

pub struct LabOffloadRequest<'a> {
    pub command: Option<LabOffloadCommand>,
    pub normalized_args: &'a [String],
    pub explicit_runner: Option<&'a str>,
    pub force_hot: bool,
    pub local_policy: LabLocalExecutionPolicy,
    pub allow_dirty_lab_workspace: bool,
    /// Skip post-materialization dependency hydration for Lab workspace exec
    /// jobs. When true, Homeboy does not run `composer install`/`npm ci`/etc. in
    /// the materialized runner workspace before the command starts (#7366).
    pub skip_deps_hydration: bool,
    pub capture_patch: bool,
    /// Human-readable flag (e.g. `--write`, `--fix`) that requested the
    /// source-tree mutation. Used to render actionable diagnostics when the
    /// remote runner finishes cleanly but returns no patch to apply.
    pub mutation_flag: Option<&'a str>,
    pub detach_after_handoff: bool,
    pub output_file_requested: bool,
    pub read_only_polling: bool,
    /// Controller-local `--output` path, when the operator requested the global
    /// JSON envelope be written to a file. Used to persist the durable agent-task
    /// run id immediately (before long-running provider execution starts) so the
    /// handle survives a local shell timeout/interruption (#5684).
    pub local_output_file: Option<&'a str>,
    pub job_overrides: LabJobOverrides,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LabExecutionContext {
    pub(crate) remote_cwd: String,
    pub(crate) source_snapshot: Option<SourceSnapshot>,
    pub(crate) path_materialization_plan: PathMaterializationPlan,
}

impl LabExecutionContext {
    pub(crate) fn new(
        remote_cwd: impl Into<String>,
        source_snapshot: Option<SourceSnapshot>,
        path_materialization_plan: PathMaterializationPlan,
    ) -> Self {
        Self {
            remote_cwd: remote_cwd.into(),
            source_snapshot,
            path_materialization_plan,
        }
    }

    pub(crate) fn workspace_mapping_ref(&self) -> Option<&'static str> {
        (!self.path_materialization_plan.entries.is_empty()).then_some("path_materialization_plan")
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LabJobOverrides {
    pub env: HashMap<String, String>,
    pub secret_env_names: Vec<String>,
    pub workspace_root: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabOffloadCommand {
    pub hot_label: &'static str,
    pub portable: bool,
    pub unsupported_reason: Option<&'static str>,
    pub source_path_mode: LabOffloadSourcePathMode,
    pub workspace_mode_policy: LabOffloadWorkspaceModePolicy,
    pub secret_env_sources: Vec<crate::command_contract::LabSecretEnvSource>,
    pub required_extensions: Vec<String>,
    pub required_capabilities: Vec<crate::command_contract::RunnerWorkloadCapability>,
    pub workload: Option<crate::command_contract::LabRigWorkloadArguments>,
    /// Routing-policy flags shared across the Lab command layers
    /// (`default_lab_offload`, `infer_source_path_tools`, `release_gate`,
    /// `requires_extension_parity`).
    pub routing_policy: crate::command_contract::LabRoutingPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabOffloadSourcePathMode {
    CwdOrPathFlag,
    RunnerResident,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabOffloadWorkspaceModePolicy {
    ChangedSinceGitElseSnapshot,
    Git,
    GitCheckoutRequired,
    RunnerResident,
}

pub enum LabOffloadOutcome {
    RunLocal {
        plan: HomeboyPlan,
        metadata: Option<serde_json::Value>,
        messages: Vec<String>,
    },
    Offloaded {
        plan: HomeboyPlan,
        stdout: String,
        stderr: String,
        exit_code: i32,
        output_file_content: Option<String>,
    },
    InFlight {
        plan: HomeboyPlan,
        stdout: String,
        stderr: String,
        exit_code: i32,
        output_file_content: Option<String>,
    },
}
