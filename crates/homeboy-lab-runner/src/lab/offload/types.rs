//! Public request/command/outcome types for Lab offload.

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
    pub durable_agent_task_plan: Option<&'a homeboy_agents::agent_task_scheduler::AgentTaskPlan>,
    /// Controller checkout selected independently of the remote command argv.
    /// This keeps process cwd in the runner job while retaining an exact local
    /// source for Git materialization and path remapping.
    pub source_path: Option<&'a std::path::Path>,
    /// Controller-derived evidence attached to staged source metadata. This is
    /// descriptive only; it cannot relax remote snapshot validation.
    pub verified_cook_baseline: Option<&'a serde_json::Value>,
    /// Select controller-bundle materialization before runner-side Git transport.
    pub require_controller_git_bundle: bool,
    /// Reuse a clean, exact-source snapshot already materialized on the selected
    /// runner instead of rebuilding the source through Git transport.
    pub reuse_compatible_snapshot: bool,
    pub job_overrides: LabJobOverrides,
}

// LabOffloadCommand, LabJobOverrides, LabOffloadOutcome, and the source/workspace
// mode aliases moved to core's lab_offload module (they are core-plan-based
// types the core lab_routing service names). Re-exported so runner-internal
// call sites resolve unchanged.
pub use homeboy_core::lab_offload::{
    LabJobOverrides, LabOffloadCommand, LabOffloadOutcome, LabOffloadSourcePathMode,
    LabOffloadWorkspaceModePolicy,
};
