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
    /// Retain failures through the runner workspace TTL lifecycle instead of
    /// deleting them at terminal completion.
    pub preserve_workspace_on_failure: bool,
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
    /// Stable controller-owned identity for detached planless commands.
    pub durable_run_id: Option<&'a str>,
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

impl<'a> LabOffloadRequest<'a> {
    #[cfg(test)]
    /// Creates a request with the neutral offload policy used by focused tests.
    /// This remains test-only so production callers must set every policy field.
    pub(crate) fn for_test(normalized_args: &'a [String]) -> Self {
        Self {
            command: None,
            normalized_args,
            explicit_runner: None,
            placement: homeboy_cli_contract::Placement::Auto,
            allow_local_fallback: false,
            allow_dirty_lab_workspace: false,
            skip_deps_hydration: false,
            preserve_workspace_on_failure: false,
            capture_patch: false,
            mutation_flag: None,
            detach_after_handoff: false,
            output_file_requested: false,
            read_only_polling: false,
            local_output_file: None,
            durable_agent_task_plan: None,
            durable_run_id: None,
            source_path: None,
            verified_cook_baseline: None,
            require_controller_git_bundle: false,
            reuse_compatible_snapshot: false,
            job_overrides: LabJobOverrides::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_has_neutral_policy() {
        let args = vec!["homeboy".to_string(), "status".to_string()];
        let request = LabOffloadRequest::for_test(&args);

        let LabOffloadRequest {
            command,
            normalized_args,
            explicit_runner,
            placement,
            allow_local_fallback,
            allow_dirty_lab_workspace,
            skip_deps_hydration,
            preserve_workspace_on_failure,
            capture_patch,
            mutation_flag,
            detach_after_handoff,
            output_file_requested,
            read_only_polling,
            local_output_file,
            durable_agent_task_plan,
            durable_run_id,
            source_path,
            verified_cook_baseline,
            require_controller_git_bundle,
            reuse_compatible_snapshot,
            job_overrides,
        } = request;

        assert!(command.is_none());
        assert_eq!(normalized_args, args);
        assert!(explicit_runner.is_none());
        assert_eq!(placement, homeboy_cli_contract::Placement::Auto);
        assert!(!allow_local_fallback);
        assert!(!allow_dirty_lab_workspace);
        assert!(!skip_deps_hydration);
        assert!(!preserve_workspace_on_failure);
        assert!(!capture_patch);
        assert!(mutation_flag.is_none());
        assert!(!detach_after_handoff);
        assert!(!output_file_requested);
        assert!(!read_only_polling);
        assert!(local_output_file.is_none());
        assert!(durable_agent_task_plan.is_none());
        assert!(durable_run_id.is_none());
        assert!(source_path.is_none());
        assert!(verified_cook_baseline.is_none());
        assert!(!require_controller_git_bundle);
        assert!(!reuse_compatible_snapshot);
        assert!(job_overrides.env.is_empty());
        assert!(job_overrides.secret_env_names.is_empty());
        assert!(job_overrides.workspace_root.is_none());
    }
}

// LabOffloadCommand, LabJobOverrides, LabOffloadOutcome, and the source/workspace
// mode aliases moved to core's lab_offload module (they are core-plan-based
// types the core lab_routing service names). Re-exported so runner-internal
// call sites resolve unchanged.
pub use homeboy_core::lab_offload::{
    LabJobOverrides, LabOffloadCommand, LabOffloadOutcome, LabOffloadSourcePathMode,
    LabOffloadWorkspaceModePolicy,
};
