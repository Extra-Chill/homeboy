//! Public request/command/outcome types for Lab offload.

pub struct LabOffloadRequest<'a> {
    /// Canonical controller-owned placement decision. This is deliberately
    /// separate from #10943's authenticated runner-job execution context.
    pub placement_decision: homeboy_lab_contract::ExecutionPlacementDecision,
    pub command: Option<LabOffloadCommand>,
    pub normalized_args: &'a [String],
    pub explicit_runner: Option<&'a str>,
    pub placement: homeboy_lab_contract::Placement,
    pub allow_local_fallback: bool,
    pub allow_dirty_lab_workspace: bool,
    /// Skip post-materialization dependency hydration for Lab workspace exec
    /// jobs. When true, Homeboy does not run `composer install`/`npm ci`/etc. in
    /// the materialized runner workspace before the command starts (#7366).
    pub skip_deps_hydration: bool,
    /// Delete a failed workspace immediately at terminal completion instead
    /// of the default bounded retention through the runner workspace TTL
    /// lifecycle.
    pub delete_workspace_on_failure: bool,
    pub capture_patch: bool,
    /// Human-readable flag (e.g. `--write`, `--fix`) that requested the
    /// source-tree mutation. Used to render actionable diagnostics when the
    /// remote runner finishes cleanly but returns no patch to apply.
    pub mutation_flag: Option<&'a str>,
    pub placement_outcome_target:
        Option<homeboy_core::lab_routing::ExecutionPlacementOutcomeTarget<'a>>,
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
    /// Stable controller-owned identity for detached planless commands.
    pub durable_run_id: Option<&'a str>,
    /// Controller checkout selected independently of the remote command argv.
    /// This keeps process cwd in the runner job while retaining an exact local
    /// source for Git materialization and path remapping.
    pub source_path: Option<&'a std::path::Path>,
    /// Immutable controller workspace identity required by a durable replay.
    pub expected_source_snapshot_identity: Option<&'a str>,
    /// Controller-derived evidence attached to staged source metadata. This is
    /// descriptive only; it cannot relax remote snapshot validation.
    pub verified_cook_baseline: Option<&'a serde_json::Value>,
    /// Select controller-bundle materialization before runner-side Git transport.
    pub require_controller_git_bundle: bool,
    pub job_overrides: LabJobOverrides,
}

impl<'a> LabOffloadRequest<'a> {
    #[cfg(test)]
    /// Creates a request with the neutral offload policy used by focused tests.
    /// This remains test-only so production callers must set every policy field.
    pub(crate) fn for_test(normalized_args: &'a [String]) -> Self {
        Self {
            placement_decision: homeboy_lab_contract::ExecutionPlacementDecision::new(
                "test",
                "1",
                homeboy_lab_contract::ExecutionPlacementIdentity {
                    repository: "test".to_string(),
                    workspace: "test".to_string(),
                    task: "test".to_string(),
                    candidate: None,
                    base: None,
                },
                homeboy_lab_contract::Placement::Auto,
                homeboy_lab_contract::ExecutionPlacementRequirement::Either,
                homeboy_lab_contract::EffectiveExecutionPlacement::Local,
                None,
                homeboy_lab_contract::ExecutionPlacementFallback {
                    local_allowed: false,
                    reason: None,
                },
                homeboy_lab_contract::ExecutionPlacementOverrideAuthorization {
                    authorized: false,
                    authority: None,
                },
            ),
            command: None,
            normalized_args,
            explicit_runner: None,
            placement: homeboy_lab_contract::Placement::Auto,
            allow_local_fallback: false,
            allow_dirty_lab_workspace: false,
            skip_deps_hydration: false,
            delete_workspace_on_failure: false,
            capture_patch: false,
            mutation_flag: None,
            placement_outcome_target: None,
            detach_after_handoff: false,
            output_file_requested: false,
            read_only_polling: false,
            local_output_file: None,
            durable_agent_task_plan: None,
            durable_run_id: None,
            source_path: None,
            expected_source_snapshot_identity: None,
            verified_cook_baseline: None,
            require_controller_git_bundle: false,
            job_overrides: LabJobOverrides::default(),
        }
    }
}

// Core-plan-based offload types owned by core's lab_offload module.
pub use homeboy_core::lab_offload::{
    LabJobOverrides, LabOffloadCommand, LabOffloadOutcome, LabOffloadSourcePathMode,
    LabOffloadWorkspaceModePolicy,
};
