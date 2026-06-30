//! Public request/command/outcome types for Lab offload.

use std::collections::HashMap;

use crate::core::plan::HomeboyPlan;

pub use crate::command_contract::LabLocalExecutionPolicy;

pub struct LabOffloadRequest<'a> {
    pub command: Option<LabOffloadCommand>,
    pub normalized_args: &'a [String],
    pub explicit_runner: Option<&'a str>,
    pub force_hot: bool,
    pub local_policy: LabLocalExecutionPolicy,
    pub allow_dirty_lab_workspace: bool,
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
    pub required_extensions: Vec<String>,
    pub requires_playwright: bool,
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
}
