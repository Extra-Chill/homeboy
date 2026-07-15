//! Public request/command/outcome types for Lab offload.

use std::collections::HashMap;

use crate::core::plan::HomeboyPlan;

pub struct LabOffloadRequest<'a> {
    pub command: Option<LabOffloadCommand>,
    pub normalized_args: &'a [String],
    pub explicit_runner: Option<&'a str>,
    pub placement: homeboy_cli_contract::Placement,
    pub allow_local_fallback: bool,
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
    /// The controller-materialized task plan to retain if this offload creates
    /// a durable agent-task record before the runner accepts its child job.
    pub durable_agent_task_plan: Option<&'a crate::core::agent_task_scheduler::AgentTaskPlan>,
    /// Controller checkout selected independently of the remote command argv.
    /// This keeps process cwd in the runner job while retaining an exact local
    /// source for Git materialization and path remapping.
    pub source_path: Option<&'a std::path::Path>,
    /// Select controller-bundle materialization before runner-side Git transport.
    pub require_controller_git_bundle: bool,
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
    pub command: crate::command_contract::LabCommandContract,
    pub required_extensions: Vec<String>,
    pub required_capabilities: Vec<crate::command_contract::RunnerWorkloadCapability>,
    pub workload: Option<crate::command_contract::LabRigWorkloadArguments>,
}

impl std::ops::Deref for LabOffloadCommand {
    type Target = crate::command_contract::LabCommandContract;

    fn deref(&self) -> &Self::Target {
        &self.command
    }
}

impl std::ops::DerefMut for LabOffloadCommand {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.command
    }
}

pub type LabOffloadSourcePathMode = crate::command_contract::LabSourcePathMode;
pub type LabOffloadWorkspaceModePolicy = crate::command_contract::LabWorkspaceModePolicy;

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
